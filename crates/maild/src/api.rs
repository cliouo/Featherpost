//! api：headless 接口（JSON-RPC over stdio / HTTP / 本地 IPC）。
//!
//! ARCHITECTURE §8：前端壳 / CLI / 脚本与守护进程之间只有 JSON-RPC 一个协议面。
//! 本插件提供 stdio（stdin/stdout 行分隔）、本地 HTTP 或本地 IPC 传输 + 最小方法集：
//! `status` / `listAccounts` / `listMessageSummaries` / `listMessages` /
//! `getMessage` / `send` / `getSettings` / `setSettings` / 通知授权；运行时设置当前只开放 rules，服务端推送
//! `message/received` / `account/state` 在 stdio、Unix domain socket 和 Windows
//! named pipe 上推送；HTTP 保持请求/响应语义，不承载推送。
//!
//! 方法按需查询服务（`ctx.get::<T>()`），未装配则返回 `SERVICE_UNAVAILABLE`，
//! 因此 api 插件本身只 inject 配置即可常驻——不因 store/transport 缺失而停摆。

use std::any::TypeId;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use cordis::{Context, Disposer, Plugin, PluginError, Service};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};

use crate::accounts::Accounts;
use crate::events::{
    AccountDetailCode, AccountState, AccountStateInfo, AgentLifecycleEvents, FrontendIntentQueued,
    LocalePreferenceChanged, MailFiltered, MailMessage, ProxyPreferenceChanged,
    ProxyPreferenceEvent,
};
use crate::frontend::is_builtin_frontend;
use crate::loader::plugin_config;
use crate::locale_preference::{LocalePreference, LocalePreferenceRequest, SetError};
use crate::notify::{NotificationAvailability, Notifier};
use crate::plugin_identity::{
    IdentitySeam, PluginKind, Principal, RotateError, SharedPrincipal, VerifyError,
};
use crate::proxy_preference::{ProxyConfigUpdate, ProxyService, SetError as ProxySetError};
use crate::reading_policy::{PatchError, PolicyPatch, ReadingPolicy, RemoteContentDecision};
use crate::rules::{RulesPatch, RulesSettings};
use crate::store::{AttachmentLookup, Store};
use crate::transport::Transport;

const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// ADR-0032 决策 1：`readAttachment` 单块原始字节上限。base64 后约 700 KiB，加应答信封
/// 落在 `MAX_RESPONSE_BYTES` 内；单测钉住最大块的整帧长度。
pub const MAX_ATTACHMENT_CHUNK_BYTES: usize = 512 * 1024;
const MAX_REQUEST_ID_BYTES: usize = 4 * 1024;
const MAX_PUSH_BODY_BYTES: usize = 64 * 1024;
const MAX_PUSH_TEXT_BYTES: usize = 8 * 1024;
const OUTPUT_QUEUE_CAPACITY: usize = 32;
const MAX_STREAM_SESSIONS: usize = 64;
const DEFAULT_HTTP_LISTEN: &str = "127.0.0.1:7394";
const MIN_BEARER_TOKEN_BYTES: usize = 32;
pub const API_TOKEN_ENV: &str = maild_brand::TOKEN_ENV;

#[cfg(unix)]
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 103;
#[cfg(windows)]
const NAMED_PIPE_PREFIX: &str = r"\\.\pipe\";

// —— JSON-RPC 2.0 协议类型 ——

/// 标准错误码（JSON-RPC 2.0）+ 应用自定义。
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// 所需服务未装配（如未配 store / transport）。
pub const SERVICE_UNAVAILABLE: i64 = -32001;
/// 请求的消息不存在。
pub const MESSAGE_NOT_FOUND: i64 = -32002;
/// ADR-0032：消息存在，但 `part_index` 不是它的附件。
pub const ATTACHMENT_NOT_FOUND: i64 = -32003;
/// ADR-0032：附件 part 存在，但字节未落库（超过内联上限或迁移前的历史缓存）。
pub const ATTACHMENT_NOT_STORED: i64 = -32004;
/// ADR-0034：草稿 id 不存在（或刚被另一个编辑者丢弃）。
pub const DRAFT_NOT_FOUND: i64 = -32005;
/// ADR-0034 决策 3：`base_version` 与当前版本不符，一个字节都没写。
/// `data.version` 给出当前版本，调用方据此重取再决定。
pub const DRAFT_CONFLICT: i64 = -32006;
/// ADR-0034 决策 5：超过草稿的字段/条数上限。`data` 指出字段与上限；**内容不被截断**。
pub const DRAFT_TOO_LARGE: i64 = -32007;
/// 账户 ID 不存在或已不可操作（ADR-0015 §9）。
pub const ACCOUNT_NOT_FOUND: i64 = -32010;
/// revision、ID 或 legacy 冲突。
pub const ACCOUNT_CONFLICT: i64 = -32011;
/// create/update 强制探测未通过。
pub const ACCOUNT_PROBE_FAILED: i64 = -32012;
/// 凭据库不支持、锁定或访问失败。
pub const CREDENTIAL_UNAVAILABLE: i64 = -32013;

/// ADR-0017：认证凭证无效、已吊销或会话已认证。应答后连接关闭（fail-closed）。
pub const AUTH_REJECTED: i64 = -32014;
/// ADR-0017：authenticate 在不支持身份的传输（stdio/HTTP）上、身份缝未装配或停摆。
pub const AUTH_UNSUPPORTED_ORIGIN: i64 = -32015;
/// ADR-0018：插件主体缺少方法所需的 capability，或方法仅限用户主体。连接不断开。
pub const METHOD_NOT_PERMITTED: i64 = -32016;
/// ADR-0019：插件安装/升级/回滚/移除/启停事务失败。message 带非秘密细节。
pub const PLUGIN_TRANSACTION_FAILED: i64 = -32017;

/// 请求来源。由传输层在 accept 时确定，客户端无法自报（ADR-0015 §9）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestOrigin {
    /// stdin/stdout 行分隔 JSON-RPC。
    Stdio,
    /// 同用户 Unix domain socket 会话。
    UnixSocket,
    /// 当前用户 Windows named pipe 会话。
    NamedPipe,
    /// 回环 HTTP + Bearer。
    Http,
}

impl RequestOrigin {
    /// 携带秘密的账户方法首版只接受同用户 Unix socket；
    /// Windows named pipe 待凭据 provider 落地后放开；HTTP 一律拒绝。
    pub fn allows_secret_mutations(self) -> bool {
        matches!(self, RequestOrigin::UnixSocket)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RequestOrigin::Stdio => "stdio",
            RequestOrigin::UnixSocket => "unix",
            RequestOrigin::NamedPipe => "named_pipe",
            RequestOrigin::Http => "http",
        }
    }
}

/// JSON-RPC 请求。`id` 为 null = 通知（不响应）；`params` 缺失 → null。
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default = "default_jsonrpc")]
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default = "default_id")]
    pub id: Value,
}

fn default_jsonrpc() -> String {
    "2.0".into()
}

fn default_id() -> Value {
    Value::Null
}

/// JSON-RPC 错误对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC 响应：result 与 error 恰一非空。
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Value,
}

impl JsonRpcResponse {
    fn ok(id: Value, result: Value) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0",
            result: Some(result),
            error: None,
            id,
        }
    }

    fn err(id: Value, error: JsonRpcError) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0",
            result: None,
            error: Some(error),
            id,
        }
    }

    /// 序列化为一行（stdio 传输用）。
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// 服务端推送（JSON-RPC 通知：无 `id`）。
#[derive(Debug, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: String,
    pub params: Value,
}

// —— 配置 ——

#[derive(Clone, Debug, Deserialize)]
pub struct ApiConfig {
    /// 传输："stdio"（默认）/ "http" / "unix" / "named_pipe"。
    #[serde(default = "default_transport")]
    pub transport: String,
    /// HTTP 监听地址；只接受 IP 字面量回环地址。
    #[serde(default = "default_http_listen")]
    pub listen: String,
    /// HTTP Bearer token；`<ENV_PREFIX>_TOKEN` 环境变量（[`API_TOKEN_ENV`]）优先。
    #[serde(default)]
    pub bearer_token: Option<String>,
}

fn default_transport() -> String {
    "stdio".into()
}

fn default_http_listen() -> String {
    DEFAULT_HTTP_LISTEN.into()
}

impl Default for ApiConfig {
    fn default() -> Self {
        ApiConfig {
            transport: default_transport(),
            listen: default_http_listen(),
            bearer_token: None,
        }
    }
}

impl Service for ApiConfig {
    fn name(&self) -> &'static str {
        "api.config"
    }
}

impl crate::loader::Merge for ApiConfig {
    fn merge(&self, fragment: &Self) -> Self {
        fragment.clone()
    }
}

/// 从 loader 使用的 TOML 结构中读取启用的 api 配置。
pub fn config_from_toml(toml_text: &str) -> Result<Option<ApiConfig>, String> {
    plugin_config(toml_text, "api").map_err(|error| error.to_string())
}

/// 配置文件内含 bearer token 时，Unix 上拒绝 group/other 可读写执行的文件。
pub fn validate_config_file_security(path: &Path, cfg: &ApiConfig) -> Result<(), String> {
    if cfg.bearer_token.is_none() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(path)
            .map_err(|e| format!("读取配置权限失败 {}: {e}", path.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "HTTP bearer token 所在配置文件权限过宽: {}（当前 {:o}，请 chmod 600）",
                path.display(),
                mode & 0o777
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn validate_http_config(cfg: &ApiConfig) -> Result<(SocketAddr, String), String> {
    let listen: SocketAddr = cfg
        .listen
        .parse()
        .map_err(|_| format!("api.listen 必须是 IP:PORT，收到: {}", cfg.listen))?;
    if !listen.ip().is_loopback() {
        return Err(format!("api HTTP 只允许回环地址，收到: {listen}"));
    }
    let token = std::env::var(API_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| cfg.bearer_token.clone())
        .ok_or_else(|| format!("api HTTP 需要 bearer_token 或 {API_TOKEN_ENV}"))?;
    validate_bearer_token(&token)?;
    Ok((listen, token))
}

/// 在创建监听器前校验 API 传输配置。
///
/// `socket` / `unix_socket` 和 `pipe` 是为早期配置保留的别名；`ipc`
/// 在 Unix 上选 Unix socket，在 Windows 上选 named pipe。
pub fn validate_api_config(cfg: &ApiConfig) -> Result<(), String> {
    match cfg.transport.as_str() {
        "stdio" => Ok(()),
        "http" => validate_http_config(cfg).map(|_| ()),
        #[cfg(unix)]
        "unix" | "unix_socket" | "socket" | "ipc" => validate_unix_socket_path(&cfg.listen),
        #[cfg(not(unix))]
        "unix" | "unix_socket" | "socket" => Err("api Unix socket 传输只在 Unix 平台可用".into()),
        #[cfg(windows)]
        "named_pipe" | "pipe" => validate_named_pipe_name(&cfg.listen),
        #[cfg(not(windows))]
        "named_pipe" | "pipe" => Err("api named pipe 传输只在 Windows 可用".into()),
        #[cfg(all(windows, not(unix)))]
        "ipc" => validate_named_pipe_name(&cfg.listen),
        #[cfg(all(not(unix), not(windows)))]
        "ipc" => Err("当前平台不支持 api IPC 传输".into()),
        other => Err(format!(
            "api 传输必须是 stdio、http、unix 或 named_pipe，收到: {other}"
        )),
    }
}

#[cfg(unix)]
fn validate_unix_socket_path(value: &str) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let path = Path::new(value);
    if value.is_empty() || !path.is_absolute() {
        return Err(format!(
            "api Unix socket listen 必须是绝对路径，收到: {value}"
        ));
    }
    if path.as_os_str().as_bytes().len() > MAX_UNIX_SOCKET_PATH_BYTES {
        return Err(format!(
            "api Unix socket 路径过长（最多 {MAX_UNIX_SOCKET_PATH_BYTES} 字节）: {value}"
        ));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("api Unix socket 路径缺少父目录: {value}"))?;
    let euid = unsafe { libc::geteuid() } as u32;
    let parent_metadata = std::fs::symlink_metadata(parent).map_err(|error| {
        format!(
            "api Unix socket 父目录不存在或无法读取 {}: {error}",
            parent.display()
        )
    })?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.file_type().is_dir() {
        return Err(format!(
            "api Unix socket 父目录必须是非符号链接目录: {}",
            parent.display()
        ));
    }
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        format!(
            "api Unix socket 父目录无法规范化 {}: {error}",
            parent.display()
        )
    })?;
    let mut ancestor = canonical_parent.as_path();
    loop {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(|error| {
            format!(
                "api Unix socket 父目录不存在或无法读取 {}: {error}",
                ancestor.display()
            )
        })?;
        if !metadata.file_type().is_dir() {
            return Err(format!(
                "api Unix socket 路径祖先必须是目录: {}",
                ancestor.display()
            ));
        }
        let mode = metadata.permissions().mode();
        if metadata.uid() != euid && metadata.uid() != 0 {
            return Err(format!(
                "api Unix socket 路径祖先必须由当前用户或 root 拥有: {}",
                ancestor.display()
            ));
        }
        let public_writable = mode & 0o022 != 0;
        let sticky_safe = mode & 0o1000 != 0 && metadata.uid() == 0;
        if public_writable && !sticky_safe {
            return Err(format!(
                "api Unix socket 路径祖先不能被任意用户写入（sticky root 目录除外）: {}",
                ancestor.display()
            ));
        }
        if ancestor == Path::new("/") {
            break;
        }
        ancestor = ancestor.parent().ok_or_else(|| {
            format!(
                "api Unix socket 路径缺少可验证的祖先: {}",
                ancestor.display()
            )
        })?;
    }
    // The leaf directory is checked before bind and must be private to the
    // current user. This closes pathname replacement through writable parents.
    if parent_metadata.uid() != euid || parent_metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "api Unix socket 父目录必须由当前用户独占（请 chmod 700）: {}",
            parent.display()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_named_pipe_name(value: &str) -> Result<(), String> {
    let Some(name) = value.strip_prefix(NAMED_PIPE_PREFIX) else {
        return Err(format!(
            r"api named pipe listen 必须以 \\.\pipe\ 开头，收到: {value}"
        ));
    };
    if name.is_empty() {
        return Err("api named pipe 名不能为空".into());
    }
    Ok(())
}

fn validate_bearer_token(token: &str) -> Result<(), String> {
    if token.len() < MIN_BEARER_TOKEN_BYTES || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(format!(
            "api bearer token 至少 {MIN_BEARER_TOKEN_BYTES} 个 ASCII 可见字符"
        ));
    }
    Ok(())
}

// —— 方法分发 ——

type HandlerResult = Result<Value, JsonRpcError>;

fn unavailable(service: &str) -> JsonRpcError {
    JsonRpcError {
        code: SERVICE_UNAVAILABLE,
        message: format!("服务未装配: {service}（检查 [plugins.{service}] 配置）"),
        data: None,
    }
}

fn invalid_params(msg: &str) -> JsonRpcError {
    JsonRpcError {
        code: INVALID_PARAMS,
        message: msg.into(),
        data: None,
    }
}

fn internal(msg: String) -> JsonRpcError {
    JsonRpcError {
        code: INTERNAL_ERROR,
        message: msg,
        data: None,
    }
}

fn valid_request_id(id: &Value) -> bool {
    matches!(id, Value::Null | Value::String(_) | Value::Number(_))
        && serde_json::to_vec(id).is_ok_and(|encoded| encoded.len() <= MAX_REQUEST_ID_BYTES)
}

/// 分发一个 JSON-RPC 请求为响应。通知（`id` 为 null）由调用方提前拦截，不响应。
///
/// 兼容入口：按 stdio 来源处理。流式/HTTP 传输必须用 [`dispatch_with_origin`]。
pub async fn dispatch(ctx: &Context, request: &JsonRpcRequest) -> JsonRpcResponse {
    dispatch_with_origin(ctx, RequestOrigin::Stdio, request).await
}

/// 携带可信来源的分发入口。
///
/// 每次调用都是独立会话（主体恒为 `User`）；需要 `authenticate` 绑定的流式传输
/// （Unix socket / named pipe）必须自持 [`SessionAuth`] 并走 [`dispatch_with_session`]。
pub async fn dispatch_with_origin(
    ctx: &Context,
    origin: RequestOrigin,
    request: &JsonRpcRequest,
) -> JsonRpcResponse {
    let mut session = SessionAuth::new(origin);
    dispatch_with_session(ctx, origin, &mut session, request).await
}

/// 会话级认证状态（ADR-0017）。每个连接一份，由 serve 循环持有；
/// `authenticate` 成功后主体绑定且连接内不可变；`AUTH_REJECTED` 应答后关闭连接。
/// 主体存于 [`SharedPrincipal`]：推送监听器持同一句柄做投递前过滤（ADR-0018）。
pub struct SessionAuth {
    principal: SharedPrincipal,
    close_after_reply: bool,
    /// 会话输出句柄（providerRegister 时借给端点写反向请求帧）。
    outbound: Option<tokio::sync::mpsc::Sender<String>>,
    /// 出站反向调用状态：pending 表 + id 分配（ADR-0021）。
    calls: Arc<crate::provider::SessionCallState>,
    /// 本连接注册的 provider 端点（连接关闭时按属主注销）。
    registered: Option<(String, Arc<crate::provider::ProviderEndpoint>)>,
}

impl SessionAuth {
    fn new(_origin: RequestOrigin) -> Self {
        Self {
            principal: SharedPrincipal::new(),
            close_after_reply: false,
            outbound: None,
            calls: crate::provider::SessionCallState::new(),
            registered: None,
        }
    }

    /// 当前会话主体的共享句柄（推送过滤用）。
    pub fn shared_principal(&self) -> SharedPrincipal {
        self.principal.clone()
    }

    /// 当前会话主体快照。
    pub fn principal(&self) -> Principal {
        self.principal.get()
    }

    /// 应答发送后是否应关闭连接。
    pub fn close_after_reply(&self) -> bool {
        self.close_after_reply
    }

    /// serve 循环建立输出通道后挂接（反向请求帧经同一写任务出站）。
    fn attach_output(&mut self, output: tokio::sync::mpsc::Sender<String>) {
        self.outbound = Some(output);
    }

    /// providerRegister 时的出站句柄。
    fn outbound_handle(&self) -> Option<tokio::sync::mpsc::Sender<String>> {
        self.outbound.clone()
    }

    /// 入站反向请求应答的路由表。
    pub(crate) fn provider_calls(&self) -> Arc<crate::provider::SessionCallState> {
        self.calls.clone()
    }

    /// providerRegister 成功后登记端点；返回被顶替的本连接旧端点（如有）。
    fn set_registered(
        &mut self,
        plugin_id: String,
        endpoint: Arc<crate::provider::ProviderEndpoint>,
    ) -> Option<(String, Arc<crate::provider::ProviderEndpoint>)> {
        self.registered.replace((plugin_id, endpoint))
    }

    /// 连接关闭时的属主注销凭据。
    fn take_registered(
        &mut self,
    ) -> Option<(String, Arc<crate::provider::ProviderEndpoint>)> {
        self.registered.take()
    }
}

// —— 方法级 capability enforcement（ADR-0018）——

/// 方法的主体策略。表外方法一律 `UserOnly`（fail-closed 默认：新增的用户方法
/// 自动不对插件主体开放；需要开放的必须在下表显式登记）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MethodPolicy {
    /// 任何主体可用（握手与认证面自身）。
    Open,
    /// 插件主体需声明对应 capability。
    Capability(&'static str),
    /// 仅限用户主体；插件主体永远拒绝。
    UserOnly,
}

fn method_policy(method: &str) -> MethodPolicy {
    match method {
        "authenticate" | "whoami" | "getAgentInfo" => MethodPolicy::Open,
        // ADR-0048 决策 2：语言偏好不敏感，后台插件也可能要按用户语言渲染自己的文字，
        // 与 `getAgentInfo` 同级；改偏好与 setSettings / setReadingPolicy 同属应用级开关域，
        // 不新增授权词。
        "getLocalePreference" => MethodPolicy::Open,
        "setLocalePreference" => MethodPolicy::Capability("settings_write"),
        // 全局代理偏好：读配置（默认不含密码）对所有主体开放；修改与连通性测试复用既有 settings_write，
        // 不新增授权词。
        "getProxyConfig" => MethodPolicy::Open,
        "setProxyConfig" | "testProxy" => MethodPolicy::Capability("settings_write"),
        "listAccounts" | "listFolders" | "listMessageSummaries" | "listMessages"
        | "getMessage" | "searchMessages" | "getThread" | "getSearchIndexStatus" => {
            // 换一个检索维度不构成新的权限面：读到的是 `listMessages` 已经能读的同一批
            // 数据（ADR-0029 决策 12）。
            MethodPolicy::Capability("messages_read")
        }
        // `status` 返回版本 + 账户状态 + 落库计数。`getAgentInfo`（Open）已给版本、
        // `listAccounts`（messages_read）已给账户状态——它不比已授权的方法多暴露什么
        // （ADR-0030 决策 8）。
        "status" => MethodPolicy::Capability("messages_read"),
        // ADR-0041 决策 4：「立即同步」只是提前一次本来就会发生的增量同步；按 UID 补拉一封
        // 服务器历史邮件与 `searchMessages(scope=server)` 读到的是同一批数据，落库只是缓存。
        // 两者都不改服务器状态，不构成 messages_read 之外的权限面。
        "syncMessages" | "fetchServerMessage" => MethodPolicy::Capability("messages_read"),
        // 纯读：查原信并按 ADR-0028 拼会话头返回草稿，不落库、不发信。能读原文才谈得上引用
        // 原文；投递仍要 messages_send。
        "prepareReply" => MethodPolicy::Capability("messages_read"),
        // ADR-0032 决策 4：附件字节属于同一封信的同一份内容——能读完整 HTML 原文与附件元数据的
        // 主体读它的字节不构成新的权限面。
        "readAttachment" => MethodPolicy::Capability("messages_read"),
        // ADR-0032 决策 4：远程内容策略是应用级开关，与 getSettings/setSettings 同域。
        "getReadingPolicy" => MethodPolicy::Capability("settings_read"),
        "setReadingPolicy" => MethodPolicy::Capability("settings_write"),
        // ADR-0034 决策 7：草稿自成一域。不并进 `messages_read`——收件箱里的信是既成事实，
        // 草稿是用户还没决定要不要发的字；也不并进 `messages_send`——能改草稿的主体可以在
        // 用户按下发送之前换掉收件人，两者谁也不包含谁。读写分词照 settings_read/_write 的形状。
        "listDrafts" | "getDraft" => MethodPolicy::Capability("drafts_read"),
        "beginAttachmentUpload"
        | "writeAttachmentUpload"
        | "finishAttachmentUpload"
        | "cancelAttachmentUpload"
        | "createDraft"
        | "updateDraft"
        | "deleteDraft" => MethodPolicy::Capability("drafts_write"),
        // M3.7 之后受管账户是发信的唯一正路，`send` 走全局 transport 反而是遗留形态。选哪个
        // 自己的账户/别名不比「能发信」更大（ADR-0030 决策 8）。
        "sendDraft" | "resolveDraftSend" | "send" | "sendFromAccount" => {
            MethodPolicy::Capability("messages_send")
        }
        "getSettings" => MethodPolicy::Capability("settings_read"),
        // 通知授权只能触发系统提示，不能授予自己权限；与 setSettings 同属应用级开关域。
        "setSettings" | "requestNotificationAuthorization" => {
            MethodPolicy::Capability("settings_write")
        }
        "getNotificationStatus" => MethodPolicy::Capability("notifications_read"),
        // M4.5-G01（ADR-0030 决策 4）：信箱状态写回自己一个词。并进 messages_read 会让任何
        // 可读主体删用户的邮件，并进 messages_send 是投递域——ADR-0027 决策 9 的判断成立，
        // 缺的只是它自己的词。
        "markMessages" | "moveMessages" | "deleteMessages" | "listPendingOps"
        | "resolvePendingOp" => MethodPolicy::Capability("mailbox_write"),
        // 受托管理面（ADR-0030 决策 4/5）：仅前端 manifest 可声明，external plugin 词表不含
        // 它们——后台插件没有理由改账户配置，更没有理由装下一个插件。
        "listAccountProfiles" | "probeAccount" | "createAccount" | "updateAccount"
        | "setAccountEnabled" | "removeAccount" => MethodPolicy::Capability("accounts_manage"),
        // ADR-0042：服务商自动发现是添加账户向导的一步；可选的公网探测只在调用方显式
        // `allow_network = true` 时发生，且受域名公网校验约束。与 probeAccount 同域。
        "discoverEmailProvider" | "listEmailPresets" => {
            MethodPolicy::Capability("accounts_manage")
        }
        "listAddresses" | "createAddress" | "updateAddress" | "removeAddress" => {
            MethodPolicy::Capability("addresses_manage")
        }
        "listExternalPlugins" | "openExternalPluginUi" | "commitPluginInstall" | "removePlugin"
        | "rollbackPlugin" | "setPluginEnabled" | "refreshPlugins"
        | "getPluginInstallRoot" | "previewPluginPackage" | "setDefaultFrontend" => {
            MethodPolicy::Capability("plugins_manage")
        }
        "listFrontends" | "claimNextOpenIntent" | "openFrontend" => {
            MethodPolicy::Capability("frontend_open")
        }
        // M4-05（ADR-0021）：transport provider 服务注册。仅 external manifest
        // 可声明该词；frontend route 翻译表不含它，前端主体天然拿不到。
        // User 主体在 handler 内显式拒绝（注册是插件主体的动作）。
        "providerRegister" => MethodPolicy::Capability("transport_provider"),
        // 提权防线（ADR-0018 决策 1，ADR-0030 决策 6 保留并加强）：持有一份凭证的主体不得
        // 铸造凭证，否则声明上限可以自己给自己改。ADR-0039 另保留原生恢复维护例外。
        "revealPluginToken" | "repairBuiltinFrontend" | "resyncBuiltinFrontend"
        | "stopManagedAgent" => MethodPolicy::UserOnly,
        // fail-closed 默认：未登记的方法对插件主体一律拒绝。
        _ => MethodPolicy::UserOnly,
    }
}

/// 服务端推送所需的 capability（ADR-0018 §3）：邮件与账户事件同属
/// `messages_read` 域（与 listAccounts/listMessages 一致）。
const EVENT_CAPABILITY: &str = "messages_read";

/// 主体是否持有某 capability。User 恒真；插件查 manifest 原词表；
/// 前端经 route 翻译表；**id 不在册 → 空表**（fail-closed）。
fn principal_has_capability(ctx: &Context, principal: &Principal, capability: &str) -> bool {
    match principal {
        Principal::User => true,
        Principal::Plugin(id) => ctx
            .get::<crate::external::ExternalPluginManager>()
            .and_then(|manager| manager.capabilities_of(id))
            .is_some_and(|caps| caps.iter().any(|cap| cap == capability)),
        Principal::Frontend(id) => ctx
            .get::<crate::frontend::FrontendBroker>()
            .and_then(|broker| broker.api_capabilities_of(id))
            .is_some_and(|caps| caps.contains(&capability)),
    }
}

/// dispatch 前置授权检查。User 主体直接放行；插件主体按策略表判定，
/// 拒绝记入身份缝计数并返回 -32016（连接不断开）。
fn ensure_principal_allowed(
    ctx: &Context,
    session: &SessionAuth,
    method: &str,
) -> Result<(), JsonRpcError> {
    let principal = session.principal();
    if matches!(principal, Principal::User) {
        return Ok(());
    }
    let denial = |missing: String| -> JsonRpcError {
        if let Some(seam) = ctx.get::<IdentitySeam>() {
            seam.record_denial(&principal.as_str());
        }
        eprintln!(
            "[maild/auth] 拒绝 {} 调用 {}（{}）",
            principal.as_str(),
            method,
            missing
        );
        JsonRpcError {
            code: METHOD_NOT_PERMITTED,
            message: format!("插件主体无权调用 {method}：{missing}"),
            data: None,
        }
    };
    match method_policy(method) {
        MethodPolicy::Open => Ok(()),
        MethodPolicy::UserOnly => Err(denial("该方法仅限用户主体".into())),
        MethodPolicy::Capability(capability) => {
            if principal_has_capability(ctx, &principal, capability) {
                Ok(())
            } else {
                Err(denial(format!("缺少 capability {capability}")))
            }
        }
    }
}

/// 流式传输的分发入口：会话状态由调用方持有，跨请求保持绑定。
pub async fn dispatch_with_session(
    ctx: &Context,
    origin: RequestOrigin,
    session: &mut SessionAuth,
    request: &JsonRpcRequest,
) -> JsonRpcResponse {
    if request.jsonrpc != "2.0"
        || request.method.is_empty()
        || !matches!(
            request.params,
            Value::Null | Value::Array(_) | Value::Object(_)
        )
        || !valid_request_id(&request.id)
    {
        return JsonRpcResponse::err(
            Value::Null,
            JsonRpcError {
                code: INVALID_REQUEST,
                message: "invalid JSON-RPC 2.0 request".into(),
                data: None,
            },
        );
    }
    // 方法级授权（ADR-0018）：先于参数与依赖校验——无权的主体连参数都不必给。
    if let Err(error) = ensure_principal_allowed(ctx, session, &request.method) {
        return JsonRpcResponse::err(request.id.clone(), error);
    }
    if matches!(request.method.as_str(), "send" | "sendFromAccount")
        && request
            .params
            .get("attachments")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|v| v.get("upload_id").is_some()))
        && !principal_has_capability(ctx, &session.principal(), "drafts_read")
    {
        return JsonRpcResponse::err(
            request.id.clone(),
            JsonRpcError {
                code: METHOD_NOT_PERMITTED,
                message: "发送草稿附件还需 drafts_read".into(),
                data: None,
            },
        );
    }
    if matches!(request.method.as_str(), "sendDraft" | "resolveDraftSend")
        && (!principal_has_capability(ctx, &session.principal(), "drafts_read")
            || !principal_has_capability(ctx, &session.principal(), "drafts_write"))
    {
        return JsonRpcResponse::err(
            request.id.clone(),
            JsonRpcError {
                code: METHOD_NOT_PERMITTED,
                message: "发送草稿需要 drafts_read/drafts_write/messages_send".into(),
                data: None,
            },
        );
    }
    if matches!(
        request.method.as_str(),
        "listPendingOps" | "resolvePendingOp"
    ) && request.params.get("summary_only").and_then(Value::as_bool) == Some(true)
        && !principal_has_capability(ctx, &session.principal(), "messages_read")
    {
        if let Some(seam) = ctx.get::<IdentitySeam>() {
            seam.record_denial(&session.principal().as_str());
        }
        return JsonRpcResponse::err(
            request.id.clone(),
            JsonRpcError {
                code: METHOD_NOT_PERMITTED,
                message: "活动邮件摘要还需 messages_read".into(),
                data: None,
            },
        );
    }
    let result = match request.method.as_str() {
        "authenticate" => {
            authenticate_session(ctx, origin, session, &request.params, request.id.is_null())
        }
        "whoami" => whoami_session(session, &request.params).await,
        "revealPluginToken" => reveal_plugin_token(ctx, origin, &request.params).await,
        "status" => status(ctx).await,
        "syncMessages" => sync_messages(ctx, &request.params).await,
        "fetchServerMessage" => fetch_server_message(ctx, &request.params).await,
        "discoverEmailProvider" => discover_email_provider(ctx, &request.params).await,
        "listEmailPresets" => list_email_presets(&request.params),
        "listAccounts" => list_accounts(ctx).await,
        "listFolders" => list_folders(ctx, &request.params).await,
        "listMessageSummaries" => list_message_summaries(ctx, &request.params).await,
        "listMessages" => list_messages(ctx, &request.params).await,
        "getMessage" => get_message(ctx, &request.params).await,
        "readAttachment" => read_attachment(ctx, &request.params).await,
        "getReadingPolicy" => get_reading_policy(ctx).await,
        "setReadingPolicy" => set_reading_policy(ctx, &request.params).await,
        "getLocalePreference" => get_locale_preference(ctx, &request.params).await,
        "setLocalePreference" => set_locale_preference(ctx, &request.params).await,
        "getProxyConfig" => get_proxy_config(ctx, origin, &request.params).await,
        "setProxyConfig" => set_proxy_config(ctx, &request.params).await,
        "testProxy" => test_proxy(ctx, &request.params).await,
        "searchMessages" => search_messages(ctx, &request.params).await,
        "getThread" => get_thread(ctx, &request.params).await,
        "getSearchIndexStatus" => search_index_status(ctx, &request.params).await,
        "markMessages" => mark_messages(ctx, &request.params).await,
        "moveMessages" => move_messages(ctx, &request.params).await,
        "deleteMessages" => delete_messages(ctx, &request.params).await,
        "listPendingOps" => list_pending_ops(ctx, &request.params).await,
        "resolvePendingOp" => resolve_pending_op(ctx, &request.params).await,
        "send" => send(ctx, &request.params).await,
        "getSettings" => get_settings(ctx).await,
        "setSettings" => set_settings(ctx, &request.params).await,
        "getNotificationStatus" => notification_status(ctx, &request.params).await,
        "requestNotificationAuthorization" => {
            request_notification_authorization(ctx, &request.params).await
        }
        "claimNextOpenIntent" => claim_next_open_intent(ctx, session, &request.params).await,
        "listFrontends" => list_frontends(ctx, &request.params).await,
        "listExternalPlugins" => list_external_plugins(ctx, &request.params).await,
        "openExternalPluginUi" => open_external_plugin_ui(ctx, &request.params).await,
        "openFrontend" => open_frontend(ctx, &request.params).await,
        "getAgentInfo" => get_agent_info(&request.params).await,
        "getPluginInstallRoot" => plugin_install_root(ctx, &request.params).await,
        "previewPluginPackage" => preview_plugin_package(ctx, &request.params).await,
        "setDefaultFrontend" => set_default_frontend(ctx, &request.params).await,
        "repairBuiltinFrontend" => repair_builtin_frontend(ctx, origin, &request.params).await,
        "resyncBuiltinFrontend" => resync_builtin_frontend(ctx, origin, &request.params).await,
        "stopManagedAgent" => stop_managed_agent(ctx, origin, &request.params),
        "commitPluginInstall" => commit_plugin_install(ctx, &request.params).await,
        "removePlugin" => remove_plugin(ctx, &request.params).await,
        "rollbackPlugin" => rollback_plugin(ctx, &request.params).await,
        "setPluginEnabled" => set_plugin_enabled(ctx, &request.params).await,
        "refreshPlugins" => refresh_plugins(ctx, &request.params).await,
        "listAccountProfiles" => list_account_profiles(ctx, &request.params).await,
        "probeAccount" => probe_account_rpc(ctx, origin, &request.params).await,
        "createAccount" => create_account_rpc(ctx, origin, &request.params).await,
        "updateAccount" => update_account_rpc(ctx, origin, &request.params).await,
        "setAccountEnabled" => set_account_enabled_rpc(ctx, &request.params).await,
        "removeAccount" => remove_account_rpc(ctx, &request.params).await,
        "sendFromAccount" => send_from_account_rpc(ctx, &request.params).await,
        "prepareReply" => prepare_reply(ctx, &request.params).await,
        "listDrafts" => list_drafts(ctx, &request.params).await,
        "getDraft" => get_draft(ctx, &request.params).await,
        "createDraft" => create_draft(ctx, &request.params).await,
        "sendDraft" => send_draft(ctx, &request.params, false).await,
        "resolveDraftSend" => send_draft(ctx, &request.params, true).await,
        "updateDraft" => update_draft(ctx, &request.params).await,
        "beginAttachmentUpload"
        | "writeAttachmentUpload"
        | "finishAttachmentUpload"
        | "cancelAttachmentUpload" => {
            attachment_upload(ctx, session, &request.method, &request.params).await
        }
        "deleteDraft" => delete_draft(ctx, &request.params).await,
        "listAddresses" => list_addresses_rpc(ctx, &request.params).await,
        "createAddress" => create_address_rpc(ctx, &request.params).await,
        "updateAddress" => update_address_rpc(ctx, &request.params).await,
        "removeAddress" => remove_address_rpc(ctx, &request.params).await,
        "providerRegister" => provider_register(ctx, origin, session, &request.params).await,
        _ => Err(JsonRpcError {
            code: METHOD_NOT_FOUND,
            message: format!("method not found: {}", request.method),
            data: None,
        }),
    };
    match result {
        Ok(v) => JsonRpcResponse::ok(request.id.clone(), v),
        Err(e) => JsonRpcResponse::err(request.id.clone(), e),
    }
}

fn parse_request(frame: &[u8]) -> Result<JsonRpcRequest, Box<JsonRpcResponse>> {
    let value = serde_json::from_slice::<Value>(frame).map_err(|e| {
        Box::new(JsonRpcResponse::err(
            Value::Null,
            JsonRpcError {
                code: PARSE_ERROR,
                message: format!("parse error: {e}"),
                data: None,
            },
        ))
    })?;
    let id = value
        .as_object()
        .and_then(|object| object.get("id"))
        .filter(|id| valid_request_id(id))
        .cloned()
        .unwrap_or(Value::Null);
    let valid = value
        .as_object()
        .map(|object| {
            object.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                && object
                    .get("method")
                    .and_then(Value::as_str)
                    .is_some_and(|method| !method.is_empty())
                && object
                    .get("params")
                    .is_none_or(|params| matches!(params, Value::Array(_) | Value::Object(_)))
                && object.get("id").is_none_or(valid_request_id)
        })
        .unwrap_or(false);
    if !valid {
        return Err(Box::new(JsonRpcResponse::err(
            id,
            JsonRpcError {
                code: INVALID_REQUEST,
                message: "invalid JSON-RPC 2.0 request".into(),
                data: None,
            },
        )));
    }
    serde_json::from_value(value).map_err(|_| {
        Box::new(JsonRpcResponse::err(
            id,
            JsonRpcError {
                code: INVALID_REQUEST,
                message: "invalid JSON-RPC 2.0 request".into(),
                data: None,
            },
        ))
    })
}

/// `status`：守护进程版本 + 账户状态 + 落库消息数。
///
/// ADR-0041 决策 6 的观测面以 additive `runtime` 对象附加：既有三个字段逐字不变。
/// 平台拿不到的口径给 `null`（例如 Linux 没有 `phys_footprint`、未装配 store）。
async fn status(ctx: &Context) -> HandlerResult {
    let (message_count, metrics) = match ctx.get::<Store>() {
        Some(store) => tokio::task::spawn_blocking(move || {
            (store.count().unwrap_or(0), Some(store.storage_metrics()))
        })
        .await
        .unwrap_or((0, None)),
        None => (0, None),
    };
    let (rss_bytes, phys_footprint_bytes) = match crate::daemon::current_process_memory_bytes() {
        Some((rss, footprint)) => (Some(rss), footprint),
        None => (None, None),
    };
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "accounts": account_states(ctx),
        "message_count": message_count,
        "runtime": {
            "rss_bytes": rss_bytes,
            "phys_footprint_bytes": phys_footprint_bytes,
            "store_db_bytes": metrics.as_ref().map(|m| m.db_bytes),
            "store_wal_bytes": metrics.as_ref().map(|m| m.wal_bytes),
            "stored_messages": metrics.as_ref().map(|m| m.message_count),
            "stored_attachment_bytes": metrics.as_ref().map(|m| m.stored_attachment_bytes),
        },
    }))
}

/// 按名字找一个当前在线循环里的账户句柄（`Accounts` 只含启用账户）。
fn receive_for(ctx: &Context, account: &str) -> Option<crate::receive::Receive> {
    ctx.get::<Accounts>().and_then(|accounts| {
        accounts
            .snapshot()
            .into_iter()
            .find(|(name, _, _)| name == account)
            .map(|(_, _, receive)| receive)
    })
}

/// `syncMessages`：唤醒收信循环立即跑一轮增量同步（ADR-0041 决策 4）。
///
/// 默认只敲门；`track` 为指定账户返回可查询的同步 ID，可选 `folder`。
async fn sync_messages(ctx: &Context, params: &Value) -> HandlerResult {
    let (account, folder, track, sync_id) = match params {
        Value::Null => (None, None, false, None),
        Value::Object(object) => {
            if object
                .keys()
                .any(|key| !["account", "folder", "track", "sync_id"].contains(&key.as_str()))
            {
                return Err(invalid_params("未知同步参数"));
            }
            let folder = optional_account(object, "folder")?;
            let track = match object.get("track") {
                None => false,
                Some(Value::Bool(track)) => *track,
                _ => return Err(invalid_params("`track` 必须是布尔值")),
            };
            let sync_id = object
                .get("sync_id")
                .map(|value| {
                    value
                        .as_u64()
                        .filter(|id| *id > 0)
                        .ok_or_else(|| invalid_params("`sync_id` 必须是正整数"))
                })
                .transpose()?;
            (optional_account(object, "account")?, folder, track, sync_id)
        }
        _ => return Err(invalid_params("参数必须是对象")),
    };
    if track || folder.is_some() || sync_id.is_some() {
        if folder
            .as_ref()
            .is_some_and(|name| !track || name.len() > MAX_FOLDER_NAME_BYTES)
            || (track && sync_id.is_some())
        {
            return Err(invalid_params("同步参数组合无效"));
        }
        let account = account.ok_or_else(|| invalid_params("必须指定账户"))?;
        let receive =
            receive_for(ctx, &account).ok_or_else(|| invalid_params("账户不存在或未启用"))?;
        if let Some(id) = sync_id {
            let done = receive.manual_sync_done(id).map_err(internal)?;
            return Ok(json!({ "done": done }));
        }
        let id = receive.start_manual_sync(folder).map_err(internal)?;
        return Ok(json!({ "sync_id": id }));
    }
    let entries = ctx
        .get::<Accounts>()
        .map(|accounts| accounts.snapshot())
        .unwrap_or_default();
    let targets: Vec<_> = entries
        .into_iter()
        .filter(|(name, _, _)| account.as_ref().is_none_or(|wanted| wanted == name))
        .collect();
    if account.is_some() && targets.is_empty() {
        return Err(invalid_params("账户不存在或未启用"));
    }
    let mut triggered = Vec::with_capacity(targets.len());
    let mut online_count = 0usize;
    for (name, _, receive) in targets {
        receive.trigger_sync();
        if receive.is_online() {
            online_count += 1;
        }
        triggered.push(name);
    }
    Ok(json!({ "triggered": triggered, "online_count": online_count }))
}

/// `fetchServerMessage`：本地有就直接返回；没有就在该账户的收信连接上按 UID 拉回并落库
/// （ADR-0041 决策 4）。服务器上也没有 → `-32002`。返回形状与 `getMessage` 一致。
async fn fetch_server_message(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object
        .keys()
        .any(|key| !["account", "folder", "uid"].contains(&key.as_str()))
    {
        return Err(invalid_params("参数只能包含 `account`、`folder` 和 `uid`"));
    }
    let text = |key: &str, max: usize| -> Result<String, JsonRpcError> {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= max)
            .map(str::to_owned)
            .ok_or_else(|| invalid_params(&format!("`{key}` 必须是非空字符串")))
    };
    let account = text("account", 256)?;
    let folder = text("folder", MAX_FOLDER_NAME_BYTES)?;
    let uid = text("uid", 10)?;
    let uid_num = uid
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0 && value.to_string() == uid)
        .ok_or_else(|| invalid_params("`uid` 必须是正整数"))?;
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;

    let lookup = || {
        let store = store.clone();
        let (account, folder, uid) = (account.clone(), folder.clone(), uid.clone());
        tokio::task::spawn_blocking(move || store.get_message(&account, Some(&folder), &uid))
    };
    let local = lookup()
        .await
        .map_err(|error| internal(format!("查询任务失败: {error}")))?
        .map_err(|error| internal(format!("查询失败: {error}")))?;
    let message = match local {
        Some(message) => message,
        None => {
            let receive = receive_for(ctx, &account)
                .ok_or_else(|| invalid_params("账户不存在或未启用"))?;
            let fetched = receive
                .fetch_message_by_uid(&folder, uid_num)
                .await
                .map_err(|detail| JsonRpcError {
                    code: SERVICE_UNAVAILABLE,
                    message: detail,
                    data: None,
                })?
                .ok_or_else(|| JsonRpcError {
                    code: MESSAGE_NOT_FOUND,
                    message: "message not found".into(),
                    data: None,
                })?;
            // 落库后的那份才带 thread_id/folder 规范名等派生字段；读不回来就用拉回的原件。
            lookup()
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .unwrap_or(fetched)
        }
    };
    let remote_content = ctx
        .get::<ReadingPolicy>()
        .map(|policy| policy.decide(&message.sender))
        .unwrap_or(RemoteContentDecision::FALLBACK);
    Ok(json!({ "message": message, "remote_content": remote_content }))
}

/// `discoverEmailProvider`：添加账户向导的服务商识别（ADR-0042）。
///
/// 本地知识库优先、零网络；`allow_network = true` 才做有界的公网可达性探测。`input`
/// 为空串时返回预设卡片列表（与 `listEmailPresets` 相同）。
async fn discover_email_provider(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object
        .keys()
        .any(|key| key != "input" && key != "allow_network")
    {
        return Err(invalid_params("参数只能包含 `input` 和 `allow_network`"));
    }
    let input = match object.get("input") {
        Some(Value::String(value)) if value.len() <= 320 => value.trim().to_owned(),
        _ => return Err(invalid_params("`input` 必须是不超过 320 字节的字符串")),
    };
    let allow_network = match object.get("allow_network") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(invalid_params("`allow_network` 必须是布尔")),
    };
    if input.is_empty() {
        return Ok(json!({ "presets": crate::autodiscover::list_presets() }));
    }
    let proxy = ctx.get::<ProxyService>().map(|service| {
        service.connector_with_context(maild_proxy::ProxyContextDefaults::mail())
    });
    let discovery =
        crate::autodiscover::discover_provider_with_proxy(&input, allow_network, proxy.as_ref())
            .await
            .map_err(|detail| invalid_params(&detail))?;
    serde_json::to_value(discovery).map_err(|error| internal(format!("序列化失败: {error}")))
}

/// `listEmailPresets`：常用服务商预设卡片（纯本地数据）。
fn list_email_presets(params: &Value) -> HandlerResult {
    require_empty_params(params)?;
    Ok(json!({ "presets": crate::autodiscover::list_presets() }))
}

/// 内核身份与协议代数（ADR-0016）。判定只依据 `protocol_version`；其余字段仅诊断展示，
/// 客户端不得让其参与逻辑。方法名是全新的：旧内核的 -32601 应答本身就是遗留判定信号。
async fn get_agent_info(params: &Value) -> HandlerResult {
    require_empty_params(params)?;
    let mut info = serde_json::Map::new();
    info.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    info.insert(
        "protocol_version".into(),
        json!(maild_client::PROTOCOL_VERSION),
    );
    info.insert("pid".into(), json!(std::process::id()));
    info.insert("started_at".into(), json!(started_at_epoch_millis()));
    // 打包脚本注入 MAILD_BUILD_REV 时才出现；开发构建省略。
    if let Some(rev) = option_env!("MAILD_BUILD_REV") {
        info.insert("build_rev".into(), json!(rev));
    }
    if let Ok(path) = std::env::current_exe() {
        info.insert(
            "executable_path".into(),
            json!(path.to_string_lossy().into_owned()),
        );
    }
    Ok(Value::Object(info))
}

/// 进程级启动时刻（epoch 毫秒）。api 插件装配时初始化；测试环境回退为首次查询时刻。
fn started_at_epoch_millis() -> u64 {
    static STARTED_AT: OnceLock<SystemTime> = OnceLock::new();
    let started = STARTED_AT.get_or_init(SystemTime::now);
    started
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn require_empty_params(params: &Value) -> Result<(), JsonRpcError> {
    match params {
        Value::Null => Ok(()),
        Value::Object(object) if object.is_empty() => Ok(()),
        _ => Err(invalid_params("参数必须为空对象")),
    }
}

/// 查询系统通知授权；只刷新 settings，不会触发授权界面。
async fn notification_status(ctx: &Context, params: &Value) -> HandlerResult {
    require_empty_params(params)?;
    let notifier = ctx.get::<Notifier>().ok_or_else(|| unavailable("notify"))?;
    let status = tokio::task::spawn_blocking(move || {
        notifier
            .refresh_status()
            .unwrap_or_else(|_| notifier.status())
    })
    .await
    .map_err(|_| internal("通知状态任务失败".into()))?;
    serde_json::to_value(status).map_err(|_| internal("通知状态序列化失败".into()))
}

/// 显式请求系统通知授权；只有 not_determined 会调用系统授权 API。
async fn request_notification_authorization(ctx: &Context, params: &Value) -> HandlerResult {
    require_empty_params(params)?;
    let notifier = ctx.get::<Notifier>().ok_or_else(|| unavailable("notify"))?;
    let status = tokio::task::spawn_blocking(move || notifier.request_authorization())
        .await
        .map_err(|_| internal("通知授权任务失败".into()))?
        .map_err(|_| internal("通知授权请求失败".into()))?;
    if status.availability != NotificationAvailability::Available {
        return Err(unavailable("macos notification identity"));
    }
    serde_json::to_value(status).map_err(|_| internal("通知状态序列化失败".into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimOpenIntentParams {
    frontend: String,
}

/// Claim one bounded navigation intent. Authentication stays at the local API transport layer.
async fn claim_next_open_intent(
    ctx: &Context,
    session: &SessionAuth,
    params: &Value,
) -> HandlerResult {
    let params: ClaimOpenIntentParams =
        serde_json::from_value(params.clone()).map_err(|_| invalid_params("frontend 参数无效"))?;
    // ADR-0031 决策 8：前端主体隐含 frontend_open，但只能消费排给自己的导航意图——
    // 替别的前端 claim 等于把用户的一次点击吞掉。user 与插件主体沿用 ADR-0014 的同信任域语义。
    if let Principal::Frontend(id) = session.principal() {
        if id != params.frontend {
            return Err(invalid_params("前端主体只能 claim 自己的 intent"));
        }
    }
    let broker = ctx
        .get::<crate::frontend::FrontendBroker>()
        .ok_or_else(|| unavailable("frontend"))?;
    let intent = broker
        .claim(&params.frontend)
        .map_err(|_| invalid_params("frontend 参数无效"))?;
    serde_json::to_value(maild_client::OpenIntentResult { intent })
        .map_err(|_| internal("前端意图序列化失败".into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenFrontendParams {
    #[serde(default)]
    frontend: Option<String>,
    intent: maild_client::OpenIntent,
}

async fn list_frontends(ctx: &Context, params: &Value) -> HandlerResult {
    require_empty_params(params)?;
    let broker = ctx
        .get::<crate::frontend::FrontendBroker>()
        .ok_or_else(|| unavailable("frontend"))?;
    serde_json::to_value(broker.frontends(ctx.get::<IdentitySeam>().as_deref()))
        .map_err(|_| internal("frontend 清单序列化失败".into()))
}

async fn list_external_plugins(ctx: &Context, params: &Value) -> HandlerResult {
    require_empty_params(params)?;
    let manager = ctx
        .get::<crate::external::ExternalPluginManager>()
        .ok_or_else(|| unavailable("external"))?;
    serde_json::to_value(manager.status(
        ctx.get::<IdentitySeam>().as_deref(),
        ctx.get::<crate::provider::ProviderRegistry>().as_deref(),
    ))
        .map_err(|_| internal("external 插件状态序列化失败".into()))
}

async fn open_external_plugin_ui(ctx: &Context, params: &Value) -> HandlerResult {
    let params: PluginIdParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("openExternalPluginUi 参数无效"))?;
    if params.kind.is_some() {
        return Err(invalid_params("openExternalPluginUi 不接受 kind"));
    }
    let manager = ctx
        .get::<crate::external::ExternalPluginManager>()
        .ok_or_else(|| unavailable("external"))?;
    serde_json::to_value(manager.open_ui(&params.id).map_err(transaction_error)?)
        .map_err(|_| internal("插件界面启动结果序列化失败".into()))
}

/// Submit a typed navigation request. Local transport authentication is the caller boundary.
async fn open_frontend(ctx: &Context, params: &Value) -> HandlerResult {
    let params: OpenFrontendParams =
        serde_json::from_value(params.clone()).map_err(|_| invalid_params("intent 参数无效"))?;
    let broker = ctx
        .get::<crate::frontend::FrontendBroker>()
        .ok_or_else(|| unavailable("frontend"))?;
    let activation_requested = broker
        .open_for(params.frontend.as_deref(), params.intent)
        .map_err(|_| invalid_params("frontend 或 intent 参数无效"))?;
    serde_json::to_value(maild_client::OpenFrontendResult {
        queued: true,
        activation_requested,
    })
    .map_err(|_| internal("前端激活结果序列化失败".into()))
}

/// `listAccounts`：账户名 + 连接状态列表。
async fn list_accounts(ctx: &Context) -> HandlerResult {
    Ok(json!({ "accounts": account_states(ctx) }))
}

/// 账户状态列表（无账户 → 空数组）。
fn account_states(ctx: &Context) -> Vec<Value> {
    match ctx.get::<Accounts>() {
        Some(accounts) => accounts
            .snapshot()
            .iter()
            .map(|(_, _, rx)| serde_json::to_value(rx.state()).unwrap_or(Value::Null))
            .collect(),
        None => Vec::new(),
    }
}

/// `listMessages`：分页（`limit` 默认 50 上限 500，`offset` 默认 0，最近在前）。
async fn list_messages(ctx: &Context, params: &Value) -> HandlerResult {
    let object = match params {
        Value::Null => None,
        Value::Object(object) => Some(object),
        _ => return Err(invalid_params("参数必须是对象")),
    };
    if object.is_some_and(|object| object.keys().any(|key| key != "limit" && key != "offset")) {
        return Err(invalid_params("参数只能包含 `limit` 和 `offset`"));
    }
    let limit = match object.and_then(|object| object.get("limit")) {
        None => 50,
        Some(value) => value
            .as_u64()
            .filter(|limit| (1..=500).contains(limit))
            .ok_or_else(|| invalid_params("`limit` 必须是 1 到 500 的整数"))?,
    } as usize;
    let offset = match object.and_then(|object| object.get("offset")) {
        None => 0,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| invalid_params("`offset` 必须是非负整数"))?,
    };
    let offset = usize::try_from(offset)
        .ok()
        .filter(|v| *v <= i64::MAX as usize)
        .ok_or_else(|| invalid_params("`offset` 过大"))?;
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let (messages, total) = tokio::task::spawn_blocking(move || {
        let messages = store.list_messages(limit, offset)?;
        let total = store.count()?;
        Ok::<_, rusqlite::Error>((messages, total))
    })
    .await
    .map_err(|e| internal(format!("查询任务失败: {e}")))?
    .map_err(|e| internal(format!("查询失败: {e}")))?;
    Ok(json!({ "messages": messages, "total": total }))
}

/// `listMessageSummaries`：按账户可选过滤的轻量分页列表，不返回正文。
async fn list_message_summaries(ctx: &Context, params: &Value) -> HandlerResult {
    let object = match params {
        Value::Null => None,
        Value::Object(object) => Some(object),
        _ => return Err(invalid_params("参数必须是对象")),
    };
    if object.is_some_and(|object| {
        object.keys().any(|key| {
            ![
                "limit", "offset", "account", "folder", "role", "seen", "flagged",
            ]
            .contains(&key.as_str())
        })
    }) {
        return Err(invalid_params("摘要查询参数无效"));
    }
    let limit = match object.and_then(|object| object.get("limit")) {
        None => 50,
        Some(value) => value
            .as_u64()
            .filter(|limit| (1..=500).contains(limit))
            .ok_or_else(|| invalid_params("`limit` 必须是 1 到 500 的整数"))?,
    } as usize;
    let offset = match object.and_then(|object| object.get("offset")) {
        None => 0,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| invalid_params("`offset` 必须是非负整数"))?,
    };
    let offset = usize::try_from(offset)
        .ok()
        .filter(|value| *value <= i64::MAX as usize)
        .ok_or_else(|| invalid_params("`offset` 过大"))?;
    let account = match object.and_then(|object| object.get("account")) {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(_) => return Err(invalid_params("`account` 必须是非空字符串或 null")),
    };
    // ADR-0026：省略 `folder` = 不按文件夹过滤，旧客户端行为逐字节不变。
    let folder = match object.and_then(|object| object.get("folder")) {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() && value.len() <= MAX_FOLDER_NAME_BYTES => {
            Some(value.clone())
        }
        Some(_) => return Err(invalid_params("`folder` 必须是非空字符串或 null")),
    };
    let role = match object.and_then(|o| o.get("role")) {
        None | Some(Value::Null) => None,
        Some(Value::String(v))
            if ["inbox", "sent", "drafts", "archive", "trash", "spam"].contains(&v.as_str()) =>
        {
            Some(v.clone())
        }
        _ => return Err(invalid_params("role 必须是已知文件夹角色")),
    };
    let boolean = |key: &str| match object.and_then(|o| o.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(v)) => Ok(Some(*v)),
        _ => Err(invalid_params("seen/flagged 必须为布尔")),
    };
    let query = crate::store::SummaryFilter {
        account,
        folder,
        role,
        seen: boolean("seen")?,
        flagged: boolean("flagged")?,
    };
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let (messages, total) =
        tokio::task::spawn_blocking(move || store.summary_page(&query, limit, offset))
            .await
            .map_err(|error| internal(format!("查询任务失败: {error}")))?
            .map_err(|error| internal(format!("查询失败: {error}")))?;
    Ok(json!({ "messages": messages, "total": total }))
}

/// IMAP mailbox 名的协议上限。RFC 3501 没有硬性规定，这里取一个宽松但有界的值。
const MAX_FOLDER_NAME_BYTES: usize = 512;

const SEARCH_KEYS: &[&str] = &[
    "query",
    "account",
    "folder",
    "from",
    "since",
    "until",
    "hasAttachments",
    "limit",
    "offset",
    "scope",
];

fn optional_account(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, JsonRpcError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(invalid_params(&format!("`{key}` 必须是非空字符串或 null"))),
    }
}

fn optional_unix_secs(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<u64>, JsonRpcError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(value.as_u64().ok_or_else(|| {
            invalid_params(&format!("`{key}` 必须是非负整数 unix 秒"))
        })?)),
    }
}

/// `searchMessages`：本地全文检索 + 结构化过滤（ADR-0029）。
///
/// `scope: "server"` 改走服务器侧 `SEARCH`，用于本地索引覆盖不到的历史。
async fn search_messages(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object.keys().any(|key| !SEARCH_KEYS.contains(&key.as_str())) {
        return Err(invalid_params(
            "参数只能包含 `query`、`account`、`folder`、`from`、`since`、`until`、\
`hasAttachments`、`limit`、`offset` 和 `scope`",
        ));
    }
    let text = match object.get("query") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(_) => return Err(invalid_params("`query` 必须是字符串或 null")),
    };
    let account = optional_account(object, "account")?;
    let folder = match object.get("folder") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() && value.len() <= MAX_FOLDER_NAME_BYTES => {
            Some(value.clone())
        }
        Some(_) => return Err(invalid_params("`folder` 必须是非空字符串或 null")),
    };
    let sender = optional_account(object, "from")?;
    let since = optional_unix_secs(object, "since")?;
    let until = optional_unix_secs(object, "until")?;
    let has_attachments = match object.get("hasAttachments") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err(invalid_params("`hasAttachments` 必须是布尔或 null")),
    };
    let limit = match object.get("limit") {
        None => 50,
        Some(value) => value
            .as_u64()
            .filter(|limit| (1..=200).contains(limit))
            .ok_or_else(|| invalid_params("`limit` 必须是 1 到 200 的整数"))?,
    } as usize;
    let offset = match object.get("offset") {
        None => 0,
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value <= i64::MAX as usize)
            .ok_or_else(|| invalid_params("`offset` 必须是非负整数"))?,
    };
    let scope = match object.get("scope") {
        None | Some(Value::Null) => "local",
        Some(Value::String(value)) if value == "local" || value == "server" => value.as_str(),
        Some(_) => return Err(invalid_params("`scope` 必须是 local 或 server")),
    };

    if scope == "server" {
        // 服务器检索必须落到一个具体信箱：`SEARCH` 只作用于当前 SELECT 的 mailbox。
        let account = account.ok_or_else(|| invalid_params("`scope=server` 必须指定 `account`"))?;
        let folder = folder.ok_or_else(|| invalid_params("`scope=server` 必须指定 `folder`"))?;
        if text.trim().is_empty() {
            return Err(invalid_params("`scope=server` 必须提供非空 `query`"));
        }
        let receive = ctx
            .get::<crate::Accounts>()
            .and_then(|accounts| {
                accounts
                    .snapshot()
                    .into_iter()
                    .find(|(name, _, _)| name == &account)
                    .map(|(_, _, receive)| receive)
            })
            .ok_or_else(|| invalid_params("账户不存在或未启用"))?;
        let messages = receive
            .server_search(&folder, &text, since, until)
            .await
            .map_err(internal)?;
        let total = messages.len() as i64;
        return Ok(json!({
            "messages": messages,
            "total": total,
            "scope": "server",
            "truncated": messages.len() >= crate::search::MAX_SERVER_SEARCH_HITS,
        }));
    }

    let query = crate::store::SearchQuery {
        text,
        account,
        folder,
        sender,
        since,
        until,
        has_attachments,
        limit,
        offset,
    };
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let (messages, total) = tokio::task::spawn_blocking(move || store.search_messages(&query))
        .await
        .map_err(|error| internal(format!("查询任务失败: {error}")))?
        .map_err(|error| internal(format!("查询失败: {error}")))?;
    Ok(json!({ "messages": messages, "total": total, "scope": "local" }))
}

/// `getThread`：一条会话的全部消息，时间正序，跨文件夹（ADR-0029 决策 7）。
async fn get_thread(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object.keys().any(|key| {
        !["account", "folder", "uid", "threadId", "limit", "offset"].contains(&key.as_str())
    }) {
        return Err(invalid_params(
            "参数只能包含 `account`、`folder`、`uid` 和 `threadId`",
        ));
    }
    let account = optional_account(object, "account")?
        .ok_or_else(|| invalid_params("`account` 必须是非空字符串"))?;
    let folder = match object.get("folder") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() && value.len() <= MAX_FOLDER_NAME_BYTES => {
            Some(value.clone())
        }
        Some(_) => return Err(invalid_params("`folder` 必须是非空字符串或 null")),
    };
    let thread_id = match object.get("threadId") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid_params("`threadId` 必须是正整数"))?,
        ),
    };
    let uid = match object.get("uid") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(_) => return Err(invalid_params("`uid` 必须是非空字符串或 null")),
    };
    if thread_id.is_none() && uid.is_none() {
        return Err(invalid_params("必须提供 `uid` 或 `threadId` 之一"));
    }
    let limit = match object.get("limit") {
        None => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|v| (1..=200).contains(v))
                .ok_or_else(|| invalid_params("limit 必须为 1..200"))? as usize,
        ),
    };
    let offset = object
        .get("offset")
        .map(|v| {
            v.as_u64()
                .filter(|v| *v <= i64::MAX as u64)
                .ok_or_else(|| invalid_params("offset 无效"))
        })
        .transpose()?
        .unwrap_or(0) as usize;
    if offset > 0 && limit.is_none() {
        return Err(invalid_params("offset 需要 limit"));
    }
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let result = tokio::task::spawn_blocking(move || {
        let thread_id = match thread_id {
            Some(id) => Some(id),
            None => store.thread_of(&account, folder.as_deref(), uid.as_deref().unwrap_or(""))?,
        };
        // 线程 0 = 该消息还没被回填索引；返回空线程而不是把整批未回填消息串在一起。
        let (messages, total) = match thread_id {
            Some(id) if id > 0 => match limit {
                Some(limit) => store.thread_page(&account, id, limit, offset)?,
                None => {
                    let messages = store.thread_summaries(&account, id)?;
                    let total = messages.len() as i64;
                    (messages, total)
                }
            },
            _ => (Vec::new(), 0),
        };
        Ok::<_, rusqlite::Error>((thread_id.unwrap_or(0), messages, total))
    })
    .await
    .map_err(|error| internal(format!("查询任务失败: {error}")))?
    .map_err(|error| internal(format!("查询失败: {error}")))?;
    Ok(json!({ "threadId": result.0, "messages": result.1, "total": result.2 }))
}

/// `getSearchIndexStatus`：回填进度（ADR-0029 决策 9）。
/// 回填期间结果是部分的，前端据此提示「索引中」而不是让用户以为邮件丢了。
async fn search_index_status(ctx: &Context, params: &Value) -> HandlerResult {
    if !matches!(params, Value::Null) && !matches!(params, Value::Object(object) if object.is_empty())
    {
        return Err(invalid_params("该方法不接受参数"));
    }
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let progress = tokio::task::spawn_blocking(move || store.search_index_status())
        .await
        .map_err(|error| internal(format!("查询任务失败: {error}")))?
        .map_err(|error| internal(format!("查询失败: {error}")))?;
    Ok(json!({ "pending": progress.pending, "done": progress.done }))
}

/// `listFolders`：按账户可选过滤的文件夹清单（ADR-0026）。
///
/// `present` 为 false 表示它不在服务器最近一次 `LIST` 里——不再同步，但消息保留。
async fn list_folders(ctx: &Context, params: &Value) -> HandlerResult {
    let object = match params {
        Value::Null => None,
        Value::Object(object) => Some(object),
        _ => return Err(invalid_params("参数必须是对象")),
    };
    if object.is_some_and(|object| object.keys().any(|key| key != "account")) {
        return Err(invalid_params("参数只能包含 `account`"));
    }
    let account = match object.and_then(|object| object.get("account")) {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(_) => return Err(invalid_params("`account` 必须是非空字符串或 null")),
    };
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let inbox_folders: std::collections::HashMap<_, _> = ctx
        .get::<Accounts>()
        .map(|accounts| {
            accounts
                .snapshot()
                .into_iter()
                .filter_map(|(name, _, receive)| receive.inbox_folders().map(|folders| (name, folders)))
                .collect()
        })
        .unwrap_or_default();
    let folders = tokio::task::spawn_blocking(move || {
        let records = store.list_folders(account.as_deref())?;
        let mut out = Vec::with_capacity(records.len());
        for record in records {
            let (total, unseen) = store.folder_counts(record.id)?;
            let mut folder = json!({
                "account": record.account,
                "name": record.name,
                "display_name": record.display_name,
                "role": record.role.map(|role| role.as_str()).unwrap_or(""),
                "present": record.present,
                "total": total,
                "unseen": unseen,
            });
            if let Some(inboxes) = inbox_folders.get(&record.account) {
                let name = Some(record.name.as_str());
                folder["is_primary"] = json!(inboxes.primary.as_deref() == name);
                folder["is_search_inbox"] = json!(inboxes.search.as_deref() == name);
            }
            out.push(folder);
        }
        Ok::<_, rusqlite::Error>(out)
    })
    .await
    .map_err(|error| internal(format!("查询任务失败: {error}")))?
    .map_err(|error| internal(format!("查询失败: {error}")))?;
    Ok(json!({ "folders": folders }))
}

/// `getMessage`：按账户和 UID 查询完整持久化消息。
async fn get_message(ctx: &Context, params: &Value) -> HandlerResult {
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("`account` 和 `uid` 必须是非空字符串"))?;
    let account = object
        .get("account")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_params("`account` 必须是非空字符串"))?;
    let uid = object
        .get("uid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_params("`uid` 必须是非空字符串"))?;
    // ADR-0026：UID 只在 mailbox 内唯一。省略 `folder` 时按固定规则消歧
    // （收件箱优先、其次落库序最早），客户端应当带上摘要里返回的 `folder`。
    let folder = match object.get("folder") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() && value.len() <= MAX_FOLDER_NAME_BYTES => {
            Some(value.clone())
        }
        Some(_) => return Err(invalid_params("`folder` 必须是非空字符串或 null")),
    };
    let account = account.to_owned();
    let uid = uid.to_owned();
    let message = tokio::task::spawn_blocking(move || {
        store.get_message(&account, folder.as_deref(), &uid)
    })
        .await
        .map_err(|e| internal(format!("查询任务失败: {e}")))?
        .map_err(|e| internal(format!("查询失败: {e}")))?
        .ok_or_else(|| JsonRpcError {
            code: MESSAGE_NOT_FOUND,
            message: "message not found".into(),
            data: None,
        })?;
    // ADR-0032 决策 5：策略结论放在 `message` 的同级——它不是这封信的事实，是此刻的策略。
    // 服务未装配时给与缺省策略一致的结论，消费者永远能读到这个键。
    let remote_content = ctx
        .get::<ReadingPolicy>()
        .map(|policy| policy.decide(&message.sender))
        .unwrap_or(RemoteContentDecision::FALLBACK);
    Ok(json!({ "message": message, "remote_content": remote_content }))
}

// —— 附件字节通道与阅读策略（ADR-0032） ——

/// `readAttachment`：按完整四元组分块读取已落库的附件字节（决策 1/2/3）。
async fn read_attachment(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    let account = object
        .get("account")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_params("`account` 必须是非空字符串"))?
        .to_owned();
    // 决策 2：字节读取不给 `getMessage` 那种省略 `folder` 的消歧便利。
    let folder = object
        .get("folder")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_FOLDER_NAME_BYTES)
        .ok_or_else(|| invalid_params("`folder` 必须是非空字符串（附件读取不做文件夹消歧）"))?
        .to_owned();
    let uid = object
        .get("uid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_ATTACHMENT_UID_BYTES)
        .ok_or_else(|| invalid_params("`uid` 必须是非空字符串"))?
        .to_owned();
    let part_index = object
        .get("part_index")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid_params("`part_index` 必须是非负整数"))?;
    let offset = match object.get("offset") {
        None | Some(Value::Null) => 0,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| invalid_params("`offset` 必须是非负整数"))?,
    };
    let length = match object.get("length") {
        None | Some(Value::Null) => MAX_ATTACHMENT_CHUNK_BYTES as u64,
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=MAX_ATTACHMENT_CHUNK_BYTES as u64).contains(value))
            .ok_or_else(|| {
                invalid_params(&format!(
                    "`length` 必须在 1 到 {MAX_ATTACHMENT_CHUNK_BYTES} 之间"
                ))
            })?,
    };
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let lookup = tokio::task::spawn_blocking(move || {
        store.read_attachment(&account, &folder, &uid, part_index)
    })
    .await
    .map_err(|e| internal(format!("查询任务失败: {e}")))?
    .map_err(|e| internal(format!("查询失败: {e}")))?;
    let mut attachment = match lookup {
        AttachmentLookup::MessageMissing => {
            return Err(JsonRpcError {
                code: MESSAGE_NOT_FOUND,
                message: "message not found".into(),
                data: None,
            })
        }
        AttachmentLookup::PartMissing => {
            return Err(JsonRpcError {
                code: ATTACHMENT_NOT_FOUND,
                message: "attachment not found".into(),
                data: None,
            })
        }
        AttachmentLookup::NotStored(_) => {
            return Err(JsonRpcError {
                code: ATTACHMENT_NOT_STORED,
                message: "attachment not stored（超过内联上限或迁移前的历史缓存）".into(),
                data: None,
            })
        }
        AttachmentLookup::Stored(attachment) => attachment,
    };
    let bytes = attachment.content.take().unwrap_or_default();
    let size = bytes.len() as u64;
    if offset > size {
        return Err(invalid_params("`offset` 超出附件大小"));
    }
    let end = offset.saturating_add(length).min(size);
    let chunk = &bytes[offset as usize..end as usize];
    use base64::Engine as _;
    Ok(json!({
        "attachment": attachment,
        "offset": offset,
        "length": end - offset,
        "data": base64::engine::general_purpose::STANDARD.encode(chunk),
        "eof": end == size,
    }))
}

/// `uid` 上限：IMAP UID 是 32 位整数，provider 的字符串 uid 也远在此内。
const MAX_ATTACHMENT_UID_BYTES: usize = 64;

fn reading_policy_unavailable() -> JsonRpcError {
    JsonRpcError {
        code: SERVICE_UNAVAILABLE,
        message: "阅读策略服务未装配（daemon 未以配置文件方式启动）".into(),
        data: None,
    }
}

fn reading_policy_response(policy: &ReadingPolicy, snapshot: crate::reading_policy::PolicySnapshot) -> Value {
    json!({ "policy": snapshot, "persisted": policy.is_ready() })
}

/// `getReadingPolicy`：远程内容默认策略与可信发件人名单（决策 5）。
async fn get_reading_policy(ctx: &Context) -> HandlerResult {
    let policy = ctx
        .get::<ReadingPolicy>()
        .ok_or_else(reading_policy_unavailable)?;
    Ok(reading_policy_response(&policy, policy.snapshot()))
}

/// `setReadingPolicy`：原子 patch；任一项非法整包 `-32602`，文件停摆 `-32001` 且不覆盖。
async fn set_reading_policy(ctx: &Context, params: &Value) -> HandlerResult {
    let patch: PolicyPatch = serde_json::from_value(params.clone())
        .map_err(|error| invalid_params(&format!("阅读策略 patch 无效: {error}")))?;
    let policy = ctx
        .get::<ReadingPolicy>()
        .ok_or_else(reading_policy_unavailable)?;
    let next = policy.apply(&patch, unix_now()).map_err(|error| match error {
        PatchError::Invalid(reason) => invalid_params(&format!("阅读策略 patch 无效: {reason}")),
        PatchError::Unavailable(reason) => JsonRpcError {
            code: SERVICE_UNAVAILABLE,
            message: format!("阅读策略文件停摆，拒绝覆盖: {reason}"),
            data: None,
        },
        PatchError::Io(reason) => internal(format!("阅读策略写入失败: {reason}")),
    })?;
    Ok(reading_policy_response(&policy, next))
}

fn locale_preference_unavailable() -> JsonRpcError {
    JsonRpcError {
        code: SERVICE_UNAVAILABLE,
        message: "语言偏好服务未装配（daemon 未以配置文件方式启动）".into(),
        data: None,
    }
}

fn locale_view_response(view: crate::locale_preference::LocaleView) -> HandlerResult {
    serde_json::to_value(view).map_err(|_| internal("语言偏好序列化失败".into()))
}

/// `getLocalePreference`：用户的有序语言偏好与解析结果（ADR-0048 决策 2）。`system` 模式下每次
/// 都重读系统首选语言。
async fn get_locale_preference(ctx: &Context, params: &Value) -> HandlerResult {
    require_empty_params(params)?;
    let preference = ctx
        .get::<LocalePreference>()
        .ok_or_else(locale_preference_unavailable)?;
    locale_view_response(preference.view())
}

/// `setLocalePreference`：整包替换；任一项非法 `-32602` 不落盘，文件停摆 `-32001` 且不覆盖。
/// 成功且 `effective` 变化时广播 [`LocalePreferenceChanged`]（→ `preferences/locale` 推送）。
async fn set_locale_preference(ctx: &Context, params: &Value) -> HandlerResult {
    let request: LocalePreferenceRequest = serde_json::from_value(params.clone())
        .map_err(|error| invalid_params(&format!("语言偏好参数无效: {error}")))?;
    crate::locale_preference::validate_request(&request)
        .map_err(|reason| invalid_params(&format!("语言偏好参数无效: {reason}")))?;
    let preference = ctx
        .get::<LocalePreference>()
        .ok_or_else(locale_preference_unavailable)?;
    let before = preference.effective();
    let view = preference.set(&request).map_err(|error| match error {
        SetError::Invalid(reason) => invalid_params(&format!("语言偏好参数无效: {reason}")),
        SetError::Unavailable(reason) => JsonRpcError {
            code: SERVICE_UNAVAILABLE,
            message: format!("语言偏好文件停摆，拒绝覆盖: {reason}"),
            data: None,
        },
        SetError::Io(reason) => internal(format!("语言偏好写入失败: {reason}")),
    })?;
    if view.effective != before {
        ctx.emit::<LocalePreferenceChanged>(view.effective.clone());
    }
    locale_view_response(view)
}

fn proxy_service_unavailable() -> JsonRpcError {
    JsonRpcError {
        code: SERVICE_UNAVAILABLE,
        message: "全局代理服务未装配（daemon 未以配置文件方式启动）".into(),
        data: None,
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetProxyConfigParams {
    #[serde(default)]
    include_secret: bool,
}

/// `getProxyConfig`：返回 `{ config, has_password, effective_system, persisted }`。
///
/// 默认剥离密码；仅本机流式 IPC 允许 `include_secret = true`（HTTP 端点一律拒绝）。
async fn get_proxy_config(ctx: &Context, origin: RequestOrigin, params: &Value) -> HandlerResult {
    let parsed: GetProxyConfigParams = if params.is_null() {
        GetProxyConfigParams::default()
    } else {
        serde_json::from_value(params.clone())
            .map_err(|error| invalid_params(&format!("代理配置查询参数无效: {error}")))?
    };
    if parsed.include_secret && matches!(origin, RequestOrigin::Http) {
        return Err(JsonRpcError {
            code: INVALID_REQUEST,
            message: "getProxyConfig 的 include_secret 仅支持本地流式 IPC 传输".into(),
            data: None,
        });
    }
    let service = ctx
        .get::<ProxyService>()
        .ok_or_else(proxy_service_unavailable)?;
    serde_json::to_value(service.view(parsed.include_secret))
        .map_err(|_| internal("代理配置序列化失败".into()))
}

/// `setProxyConfig`：更新并原子落盘全局代理配置。
///
/// 省略 `password` 保留已存密码；传入新 `password` 替换；`clear_password: true` 清除密码。
/// 变更成功后递增 generation 并向所有流式连接推送 `preferences/proxy`（不含密码）。
async fn set_proxy_config(ctx: &Context, params: &Value) -> HandlerResult {
    let update: ProxyConfigUpdate = serde_json::from_value(params.clone())
        .map_err(|error| invalid_params(&format!("代理配置参数无效: {error}")))?;
    let service = ctx
        .get::<ProxyService>()
        .ok_or_else(proxy_service_unavailable)?;
    let (view, event, changed) = service.set(&update).map_err(|error| match error {
        ProxySetError::Invalid(reason) => invalid_params(&format!("代理配置参数无效: {reason}")),
        ProxySetError::Unavailable(reason) => JsonRpcError {
            code: SERVICE_UNAVAILABLE,
            message: format!("代理配置文件停摆，拒绝覆盖: {reason}"),
            data: None,
        },
        ProxySetError::Io(reason) => internal(format!("代理配置写入失败: {reason}")),
    })?;
    if changed {
        ctx.emit::<ProxyPreferenceChanged>(event);
    }
    serde_json::to_value(view).map_err(|_| internal("代理配置序列化失败".into()))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestProxyParams {
    #[serde(default)]
    config: Option<ProxyConfigUpdate>,
    #[serde(default)]
    use_saved_password: bool,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    scheme: Option<maild_proxy::TargetScheme>,
}

/// `testProxy`：测试当前活动代理或未保存草稿到目标的连通性，不落盘、不修改运行状态。
async fn test_proxy(ctx: &Context, params: &Value) -> HandlerResult {
    let parsed: TestProxyParams = if params.is_null() {
        TestProxyParams::default()
    } else {
        serde_json::from_value(params.clone())
            .map_err(|error| invalid_params(&format!("代理测试参数无效: {error}")))?
    };
    let service = ctx
        .get::<ProxyService>()
        .ok_or_else(proxy_service_unavailable)?;
    let test_config = match &parsed.config {
        Some(draft) => service
            .resolve_draft(draft, parsed.use_saved_password)
            .map_err(|reason| invalid_params(&format!("代理草稿配置无效: {reason}")))?,
        None => service.snapshot(),
    };
    let target = match parsed.target.as_deref() {
        Some(raw) => maild_proxy::ProxyTarget::parse_host_port(raw, parsed.scheme)
            .map_err(|reason| invalid_params(&format!("测试目标无效: {reason}")))?,
        None => {
            let (host, port, inferred_scheme) = ctx
                .get::<crate::accounts::AccountManager>()
                .and_then(|manager| manager.first_enabled_imap_target())
                .ok_or_else(|| {
                    invalid_params("未指定测试目标且没有已启用的 IMAP 账户，请提供 target（例如 imap.example.com:993）")
                })?;
            maild_proxy::ProxyTarget::new(host, port, parsed.scheme.unwrap_or(inferred_scheme))
        }
    };

    let connector = service.connector_for(test_config);
    let planned_route = connector.resolve(&target).describe();
    let started = std::time::Instant::now();
    match connector
        .connect(&target, maild_proxy::ConnectOptions::test_default())
        .await
    {
        Ok((stream, resolved)) => {
            drop(stream);
            let latency_ms = started.elapsed().as_millis() as u64;
            Ok(json!({
                "ok": true,
                "route": resolved.describe(),
                "stage": "ready",
                "latency_ms": latency_ms,
                "detail_code": Value::Null,
                "detail": "",
            }))
        }
        Err(error) => {
            let latency_ms = started.elapsed().as_millis() as u64;
            let (stage, detail_code) = match error.kind {
                maild_proxy::ProxyErrorKind::ProxyConnectFailed
                | maild_proxy::ProxyErrorKind::ProxyTimeout => {
                    ("proxy_connect", AccountDetailCode::ProxyConnectFailed)
                }
                maild_proxy::ProxyErrorKind::AuthRequired => {
                    ("proxy_auth", AccountDetailCode::ProxyAuthFailed)
                }
                maild_proxy::ProxyErrorKind::TunnelFailed
                | maild_proxy::ProxyErrorKind::InvalidConfig => {
                    ("proxy_tunnel", AccountDetailCode::ProxyTunnelFailed)
                }
                maild_proxy::ProxyErrorKind::DirectConnectTimeout => {
                    ("connect", AccountDetailCode::ConnectTimeout)
                }
                maild_proxy::ProxyErrorKind::DirectConnectFailed => {
                    ("connect", AccountDetailCode::ConnectFailed)
                }
            };
            Ok(json!({
                "ok": false,
                "route": planned_route,
                "stage": stage,
                "latency_ms": latency_ms,
                "detail_code": detail_code.as_str(),
                "detail": crate::probe::sanitize_detail(&error.public_message()),
            }))
        }
    }
}

/// 进程内读 `effective`（ADR-0048 决策 2）。没有装配偏好服务（纯 store 装配、测试）时回落
/// [`FALLBACK_LANGUAGE`](crate::locale_preference::FALLBACK_LANGUAGE)，即今天的中文行为。
fn effective_locales(ctx: &Context) -> Vec<String> {
    ctx.get::<LocalePreference>()
        .map(|preference| preference.effective())
        .unwrap_or_else(|| vec![crate::locale_preference::FALLBACK_LANGUAGE.to_owned()])
}

// —— 草稿（ADR-0034）——

use crate::drafts::{generate_draft_id, validate_patch, DraftInvalid, DraftPatch};
use crate::store::DraftWrite;

fn draft_not_found() -> JsonRpcError {
    JsonRpcError {
        code: DRAFT_NOT_FOUND,
        message: "draft not found".into(),
        data: None,
    }
}

/// 上限拒绝：**不截断**（决策 5）。`data` 说清是哪个字段、上限是多少，
/// 好让前端把话说准，而不是笼统地「保存失败」。
fn draft_too_large(invalid: &DraftInvalid) -> JsonRpcError {
    JsonRpcError {
        code: DRAFT_TOO_LARGE,
        message: invalid.to_string(),
        data: Some(json!({ "field": invalid.field(), "limit": invalid.limit() })),
    }
}

/// store 的写入结果 → JSON-RPC。附件的三种「拿不到」复用 `readAttachment` 的码，
/// 前端不用为草稿学第二套（决策 6）。
fn draft_write_error(outcome: DraftWrite) -> JsonRpcError {
    match outcome {
        DraftWrite::Saved(_) => internal("草稿写入成功却走进了错误分支".to_string()),
        DraftWrite::NotFound => draft_not_found(),
        DraftWrite::SendLocked => JsonRpcError {
            code: -32021,
            message: "draft is locked by its delivery state".into(),
            data: None,
        },
        DraftWrite::Conflict { version } => JsonRpcError {
            code: DRAFT_CONFLICT,
            message: "draft changed elsewhere（base_version 已过期，未写入任何内容）".into(),
            data: Some(json!({ "version": version })),
        },
        DraftWrite::LimitReached { limit } => JsonRpcError {
            code: DRAFT_TOO_LARGE,
            message: format!("该账户的草稿数已达上限 {limit}"),
            data: Some(json!({ "field": "drafts", "limit": limit })),
        },
        DraftWrite::AttachmentMessageMissing { position } => JsonRpcError {
            code: MESSAGE_NOT_FOUND,
            message: "message not found（附件引用指向的邮件不在本地缓存中）".into(),
            data: Some(json!({ "attachment": position })),
        },
        DraftWrite::AttachmentPartMissing { position } => JsonRpcError {
            code: ATTACHMENT_NOT_FOUND,
            message: "attachment not found".into(),
            data: Some(json!({ "attachment": position })),
        },
        DraftWrite::AttachmentNotStored { position } => JsonRpcError {
            code: ATTACHMENT_NOT_STORED,
            message: "attachment not stored（字节未落库的 part 不能引用进草稿）".into(),
            data: Some(json!({ "attachment": position })),
        },
    }
}

fn upload_error(error: crate::uploads::UploadError) -> JsonRpcError {
    use crate::uploads::UploadError::*;
    match error {
        Missing => JsonRpcError {
            code: -32008,
            message: "upload not found or expired".into(),
            data: None,
        },
        Invalid(message) => invalid_params(message),
        Limit(field, limit) => JsonRpcError {
            code: DRAFT_TOO_LARGE,
            message: "attachment limit exceeded".into(),
            data: Some(json!({"field": field, "limit": limit})),
        },
        State(offset, size) => JsonRpcError {
            code: -32009,
            message: "upload offset or size mismatch".into(),
            data: Some(json!({"offset": offset, "size": size})),
        },
        Draft(outcome) => draft_write_error(outcome),
        Io(_) | Sql(_) => internal("附件存储失败，请重试或取消上传".into()),
    }
}

async fn attachment_upload(
    ctx: &Context,
    session: &SessionAuth,
    method: &str,
    params: &Value,
) -> HandlerResult {
    use base64::Engine;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Begin {
        draft_id: String,
        filename: String,
        mime_type: String,
        size: u64,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Write {
        upload_id: String,
        offset: u64,
        data: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Finish {
        upload_id: String,
        base_version: u64,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Cancel {
        upload_id: String,
    }
    let owner = session.principal().as_str();
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let bad = |_| invalid_params("附件上传参数无效");
    match method {
        "beginAttachmentUpload" => {
            let p: Begin = serde_json::from_value(params.clone()).map_err(bad)?;
            if p.draft_id.is_empty() || p.draft_id.len() > 64 {
                return Err(invalid_params("draft_id 无效"));
            }
            let result = tokio::task::spawn_blocking(move || {
                store.begin_attachment_upload(
                    &owner,
                    &p.draft_id,
                    &p.filename,
                    &p.mime_type,
                    p.size,
                )
            })
            .await
            .map_err(|_| internal("上传任务失败".into()))?
            .map_err(upload_error)?;
            Ok(json!({"upload": result}))
        }
        "writeAttachmentUpload" => {
            let p: Write = serde_json::from_value(params.clone()).map_err(bad)?;
            if p.data.len() > crate::uploads::CHUNK_BYTES.div_ceil(3) * 4 {
                return Err(invalid_params("上传块超过 512 KiB"));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(p.data)
                .map_err(|_| invalid_params("data 必须是 base64"))?;
            let result = tokio::task::spawn_blocking(move || {
                store.write_attachment_upload(&owner, &p.upload_id, p.offset, &bytes)
            })
            .await
            .map_err(|_| internal("上传任务失败".into()))?
            .map_err(upload_error)?;
            Ok(json!({"upload": result}))
        }
        "finishAttachmentUpload" => {
            let p: Finish = serde_json::from_value(params.clone()).map_err(bad)?;
            if p.base_version == 0 || p.base_version > i64::MAX as u64 {
                return Err(invalid_params("base_version 无效"));
            }
            let result = tokio::task::spawn_blocking(move || {
                store.finish_attachment_upload(&owner, &p.upload_id, p.base_version, unix_now())
            })
            .await
            .map_err(|_| internal("上传任务失败".into()))?
            .map_err(upload_error)?;
            Ok(json!({"draft": result}))
        }
        _ => {
            let p: Cancel = serde_json::from_value(params.clone()).map_err(bad)?;
            tokio::task::spawn_blocking(move || {
                store.cancel_attachment_upload(&owner, &p.upload_id)
            })
            .await
            .map_err(|_| internal("上传任务失败".into()))?
            .map_err(upload_error)?;
            Ok(json!({"cancelled": true}))
        }
    }
}

fn draft_id_param(params: &Value) -> Result<String, JsonRpcError> {
    params
        .as_object()
        .and_then(|object| object.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64)
        .map(str::to_owned)
        .ok_or_else(|| invalid_params("`id` 必须是非空字符串"))
}

/// 从请求对象里摘出补丁字段。未知键一律拒绝——写错字段名不该表现为「保存成功但没生效」。
fn draft_patch_from(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
) -> Result<DraftPatch, JsonRpcError> {
    let mut fields = serde_json::Map::new();
    for (key, value) in object {
        if allowed.contains(&key.as_str()) {
            continue;
        }
        fields.insert(key.clone(), value.clone());
    }
    serde_json::from_value(Value::Object(fields))
        .map_err(|error| invalid_params(&format!("草稿字段无效: {error}")))
}

/// `listDrafts`：摘要页，正文不进列表。
async fn list_drafts(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object
        .keys()
        .any(|key| key != "account" && key != "limit" && key != "offset")
    {
        return Err(invalid_params("参数只能包含 `account`、`limit` 和 `offset`"));
    }
    let account = match object.get("account") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(_) => return Err(invalid_params("`account` 必须是非空字符串或 null")),
    };
    let limit = match object.get("limit") {
        None | Some(Value::Null) => 50,
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=200).contains(value))
            .ok_or_else(|| invalid_params("`limit` 必须在 1 到 200 之间"))?,
    } as usize;
    let offset = match object.get("offset") {
        None | Some(Value::Null) => 0,
        Some(value) => value
            .as_u64()
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or_else(|| invalid_params("`offset` 必须是非负整数"))?,
    } as usize;
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let (drafts, total) = tokio::task::spawn_blocking(move || {
        store.list_drafts(account.as_deref(), limit, offset)
    })
    .await
    .map_err(|error| internal(format!("查询任务失败: {error}")))?
    .map_err(|error| internal(format!("查询失败: {error}")))?;
    Ok(json!({ "drafts": drafts, "total": total }))
}

/// `getDraft`：一份完整草稿，附件引用带**现查的** `stored` 位。
async fn get_draft(ctx: &Context, params: &Value) -> HandlerResult {
    let id = draft_id_param(params)?;
    if params
        .as_object()
        .is_some_and(|object| object.keys().any(|key| key != "id"))
    {
        return Err(invalid_params("参数只能包含 `id`"));
    }
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let draft = tokio::task::spawn_blocking(move || store.get_draft(&id))
        .await
        .map_err(|error| internal(format!("查询任务失败: {error}")))?
        .map_err(|error| internal(format!("查询失败: {error}")))?
        .ok_or_else(draft_not_found)?;
    Ok(json!({ "draft": draft }))
}

/// `createDraft`：新建。`account` 必填；其余字段是同一套补丁语义。
async fn create_draft(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    let account = object
        .get("account")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .ok_or_else(|| invalid_params("`account` 必须是非空字符串"))?
        .to_owned();
    let patch = draft_patch_from(object, &["account"])?;
    validate_patch(&patch, &crate::drafts::Draft::default()).map_err(|invalid| match invalid {
        DraftInvalid::ControlCharacter { .. } => invalid_params(&invalid.to_string()),
        other => draft_too_large(&other),
    })?;
    // 账户管理器在场时校验账户存在：写一份发不出去的草稿没有意义。
    if let Some(manager) = ctx.get::<crate::accounts::AccountManager>() {
        if !manager.has_account(&account) {
            return Err(JsonRpcError {
                code: ACCOUNT_NOT_FOUND,
                message: format!("account not found: {account}"),
                data: None,
            });
        }
    }
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let id = generate_draft_id();
    let now = unix_now();
    let outcome = tokio::task::spawn_blocking(move || store.create_draft(&id, &account, &patch, now))
        .await
        .map_err(|error| internal(format!("写入任务失败: {error}")))?
        .map_err(|error| internal(format!("写入失败: {error}")))?;
    match outcome {
        DraftWrite::Saved(draft) => Ok(json!({ "draft": draft })),
        other => Err(draft_write_error(other)),
    }
}

/// `updateDraft`：带 `base_version` 的补丁（决策 3/4）。版本不符 → `-32006`，什么都不写。
async fn update_draft(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    let id = draft_id_param(params)?;
    let base_version = object
        .get("base_version")
        .and_then(Value::as_u64)
        .filter(|value| *value >= 1)
        .ok_or_else(|| invalid_params("`base_version` 必须是不小于 1 的整数"))?;
    let patch = draft_patch_from(object, &["id", "base_version"])?;
    if let Some(account) = &patch.account {
        if account.is_empty()
            || ctx
                .get::<crate::accounts::AccountManager>()
                .is_some_and(|m| !m.has_account(account))
        {
            return Err(invalid_params("发件账户不存在"));
        }
    }
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    // 上限校验要看**合并后**的收件人总数，因此先取当前草稿。
    let current = {
        let store = Arc::clone(&store);
        let id = id.clone();
        tokio::task::spawn_blocking(move || store.get_draft(&id))
            .await
            .map_err(|error| internal(format!("查询任务失败: {error}")))?
            .map_err(|error| internal(format!("查询失败: {error}")))?
            .ok_or_else(draft_not_found)?
    };
    validate_patch(&patch, &current).map_err(|invalid| match invalid {
        DraftInvalid::ControlCharacter { .. } => invalid_params(&invalid.to_string()),
        other => draft_too_large(&other),
    })?;
    let now = unix_now();
    let outcome =
        tokio::task::spawn_blocking(move || store.update_draft(&id, base_version, &patch, now))
            .await
            .map_err(|error| internal(format!("写入任务失败: {error}")))?
            .map_err(|error| internal(format!("写入失败: {error}")))?;
    match outcome {
        DraftWrite::Saved(draft) => Ok(json!({ "draft": draft })),
        other => Err(draft_write_error(other)),
    }
}

/// `deleteDraft`：丢弃。`base_version` 可选——带上它就是「除非它在别处改过」。
async fn delete_draft(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object
        .keys()
        .any(|key| key != "id" && key != "base_version")
    {
        return Err(invalid_params("参数只能包含 `id` 和 `base_version`"));
    }
    let id = draft_id_param(params)?;
    let base_version = match object.get("base_version") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|value| *value >= 1)
                .ok_or_else(|| invalid_params("`base_version` 必须是不小于 1 的整数"))?,
        ),
    };
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let outcome = tokio::task::spawn_blocking(move || store.delete_draft(&id, base_version))
        .await
        .map_err(|error| internal(format!("写入任务失败: {error}")))?
        .map_err(|error| internal(format!("写入失败: {error}")))?;
    match outcome {
        DraftWrite::Saved(draft) => Ok(json!({ "deleted": true, "draft": draft })),
        other => Err(draft_write_error(other)),
    }
}

use crate::compose;
use crate::folders::FolderRole;
use crate::pending::{MessageFlag, OpState};

/// 写路径共用的当前时刻（unix 秒）。
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

/// 一次写入请求的 uid 上限：请求体与单事务都要有界。
const MAX_WRITE_UIDS: usize = 500;

/// 解析写方法共用的 `{account, folder, uids}`。
fn parse_write_target(params: &Value) -> Result<(String, String, Vec<String>), JsonRpcError> {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    let account = object
        .get("account")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_params("`account` 必须是非空字符串"))?
        .to_owned();
    let folder = object
        .get("folder")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_FOLDER_NAME_BYTES)
        .ok_or_else(|| invalid_params("`folder` 必须是非空字符串"))?
        .to_owned();
    let uids = object
        .get("uids")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_params("`uids` 必须是数组"))?;
    if uids.is_empty() || uids.len() > MAX_WRITE_UIDS {
        return Err(invalid_params(
            "`uids` 必须是 1 到 500 项",
        ));
    }
    let mut parsed = Vec::with_capacity(uids.len());
    for uid in uids {
        let uid = uid
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= 20)
            .ok_or_else(|| invalid_params("`uids` 每项必须是非空字符串"))?;
        parsed.push(uid.to_owned());
    }
    Ok((account, folder, parsed))
}

/// 入队后敲醒该账户的收信循环。账户离线时是空操作——op 留在库里等下次连接。
fn wake_account_writes(ctx: &Context, account: &str) {
    let Some(accounts) = ctx.get::<crate::Accounts>() else {
        return;
    };
    for (name, _, receive) in accounts.snapshot() {
        if name == account {
            receive.wake_writes();
        }
    }
}

/// `markMessages`：置位或取消一个 IMAP 标记（ADR-0027）。本地立刻生效，写回排队。
async fn mark_messages(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object.keys().any(|key| {
        key != "account" && key != "folder" && key != "uids" && key != "flag" && key != "value"
    }) {
        return Err(invalid_params(
            "参数只能包含 `account`、`folder`、`uids`、`flag` 和 `value`",
        ));
    }
    let (account, folder, uids) = parse_write_target(params)?;
    let flag = object
        .get("flag")
        .and_then(Value::as_str)
        .and_then(MessageFlag::parse)
        .ok_or_else(|| {
            invalid_params("`flag` 必须是 seen / flagged / answered / deleted 之一")
        })?;
    let value = object
        .get("value")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid_params("`value` 必须是布尔"))?;

    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let now = unix_now();
    let queued = tokio::task::spawn_blocking(move || {
        store.queue_flag_receipt(&account, &folder, &uids, flag, value, now)
    })
    .await
    .map_err(|error| internal(format!("入队任务失败: {error}")))?
    .map_err(|error| internal(format!("入队失败: {error}")))?;

    let account = params["account"].as_str().unwrap_or_default().to_owned();
    wake_account_writes(ctx, &account);
    Ok(
        json!({ "ok": true, "queued": queued.queued_uids.len(), "queued_uids":queued.queued_uids, "unchanged_uids":queued.unchanged_uids, "missing_uids":queued.missing_uids }),
    )
}

/// `moveMessages`：移动到另一个文件夹。源文件夹立刻不再显示它们（ADR-0027 决策 5）。
async fn move_messages(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object
        .keys()
        .any(|key| key != "account" && key != "folder" && key != "uids" && key != "target")
    {
        return Err(invalid_params(
            "参数只能包含 `account`、`folder`、`uids` 和 `target`",
        ));
    }
    let (account, folder, uids) = parse_write_target(params)?;
    let target = object
        .get("target")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_FOLDER_NAME_BYTES)
        .ok_or_else(|| invalid_params("`target` 必须是非空字符串"))?
        .to_owned();
    if target == folder {
        return Err(invalid_params("`target` 不能是源文件夹"));
    }
    queue_move(ctx, account, folder, uids, target).await
}

/// `deleteMessages`：有废纸篓就移进废纸篓，否则落 `\Deleted`（ADR-0027 决策 4）。
/// 判定在**入队时**完成，因此队列里每条 op 的可重放性在那一刻就已确定。
async fn delete_messages(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object
        .keys()
        .any(|key| key != "account" && key != "folder" && key != "uids")
    {
        return Err(invalid_params("参数只能包含 `account`、`folder` 和 `uids`"));
    }
    let (account, folder, uids) = parse_write_target(params)?;
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let lookup_account = account.clone();
    let trash = tokio::task::spawn_blocking(move || store.list_folders(Some(&lookup_account)))
        .await
        .map_err(|error| internal(format!("查询任务失败: {error}")))?
        .map_err(|error| internal(format!("查询失败: {error}")))?
        .into_iter()
        .find(|record| record.role == Some(FolderRole::Trash))
        .map(|record| record.name);

    match trash {
        Some(trash) if trash != folder => queue_move(ctx, account, folder, uids, trash).await,
        _ => {
            // 已经在废纸篓里，或者这个账户根本没有废纸篓：走 RFC 3501 的删除模型。
            let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
            let now = unix_now();
            let wake_for = account.clone();
            let queued = tokio::task::spawn_blocking(move || {
                store.queue_flag_receipt(&account, &folder, &uids, MessageFlag::Deleted, true, now)
            })
            .await
            .map_err(|error| internal(format!("入队任务失败: {error}")))?
            .map_err(|error| internal(format!("入队失败: {error}")))?;
            wake_account_writes(ctx, &wake_for);
            Ok(
                json!({ "ok": true, "queued": queued.queued_uids.len(), "queued_uids":queued.queued_uids, "unchanged_uids":queued.unchanged_uids, "missing_uids":queued.missing_uids, "mode": "flag" }),
            )
        }
    }
}

async fn queue_move(
    ctx: &Context,
    account: String,
    folder: String,
    uids: Vec<String>,
    target: String,
) -> HandlerResult {
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let now = unix_now();
    let wake_for = account.clone();
    let queued = tokio::task::spawn_blocking(move || {
        store.queue_move_receipt(&account, &folder, &uids, &target, now)
    })
    .await
    .map_err(|error| internal(format!("入队任务失败: {error}")))?
    .map_err(|error| internal(format!("入队失败: {error}")))?;
    wake_account_writes(ctx, &wake_for);
    Ok(
        json!({ "ok": true, "queued": queued.queued_uids.len(), "queued_uids":queued.queued_uids, "unchanged_uids":queued.unchanged_uids, "missing_uids":queued.missing_uids, "mode": "move" }),
    )
}

fn pending_op_json(op: &crate::pending::PendingOp) -> Value {
    json!({
        "id": op.id,
        "account": op.account,
        "folder": op.folder,
        "uid": op.uid,
        "kind": op.kind.as_str(),
        "payload": op.payload,
        "state": op.state.as_str(),
        "attempts": op.attempts,
        "created_at": op.created_at,
        "updated_at": op.updated_at,
        "detail": op.detail,
        // 「结果未知」对不可重放的操作是终态，必须由用户裁决（ADR-0027 决策 2）。
        "needs_decision": op.state == OpState::Unknown
            || (op.state == OpState::Failed && !op.detail.is_empty()),
    })
}

fn pending_op_summary_json(op: &crate::pending::PendingOp, subject: Option<&str>) -> Value {
    let mut value = pending_op_json(op);
    value.as_object_mut().unwrap().remove("payload");
    value["subject"] = json!(subject);
    // ADR-0048 决策 3：摘要的 `detail` 是内核拼的固定句子，另给稳定 `detail_code` 让前端按界面
    // 语言成文；`detail` 照旧保留，老前端不受影响。
    // L6：服务器命令失败（`APPEND 失败: …` 等）另给 `server_rejected` 与去掉前缀的服务器原文
    // `server_detail`（有界脱敏，作为数据展示）；其他失败仍是 `rejected`。
    let server_text = crate::receive::op_failure_server_text(&op.folder, &op.detail);
    const REJECTED: &str = "服务器明确拒绝或重试次数已达上限，请核对账户与目标文件夹。";
    let (detail, code) = match op.state {
        OpState::Failed if op.detail == "用户放弃" => ("用户放弃", Some("discarded")),
        OpState::Failed if server_text.is_some() => (REJECTED, Some("server_rejected")),
        OpState::Failed => (REJECTED, Some("rejected")),
        OpState::Unknown => ("未收到最终回执，请先核对目标文件夹。", Some("unconfirmed")),
        _ => ("", None),
    };
    value["detail"] = json!(detail);
    if let Some(code) = code {
        value["detail_code"] = json!(code);
        if let Some(text) = server_text.map(crate::probe::sanitize_detail) {
            if !text.is_empty() {
                value["server_detail"] = json!(text);
            }
        }
    }
    if op.kind != crate::pending::OpKind::Append {
        if let Ok(payload) = serde_json::from_str::<Value>(&op.payload) {
            for key in ["target", "flag", "value"] {
                if let Some(v) = payload.get(key) {
                    value[key] = v.clone();
                }
            }
        }
    }
    value
}

/// `listPendingOps`：写队列观测。默认只返回未落定的。
async fn list_pending_ops(ctx: &Context, params: &Value) -> HandlerResult {
    let object = match params {
        Value::Null => None,
        Value::Object(object) => Some(object),
        _ => return Err(invalid_params("参数必须是对象")),
    };
    if object.is_some_and(|object| {
        object.keys().any(|key| {
            ![
                "account",
                "include_settled",
                "summary_only",
                "limit",
                "before_id",
            ]
            .contains(&key.as_str())
        })
    }) {
        return Err(invalid_params("参数只能包含 `account` 和 `include_settled`"));
    }
    let account = match object.and_then(|object| object.get("account")) {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(_) => return Err(invalid_params("`account` 必须是非空字符串或 null")),
    };
    let include_settled = match object.and_then(|object| object.get("include_settled")) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(invalid_params("`include_settled` 必须是布尔")),
    };
    let summary = match object.and_then(|o| o.get("summary_only")) {
        None => false,
        Some(Value::Bool(v)) => *v,
        _ => return Err(invalid_params("summary_only 必须为布尔")),
    };
    let limit = match object.and_then(|o| o.get("limit")) {
        None => 200,
        Some(v) => v
            .as_u64()
            .filter(|v| (1..=200).contains(v))
            .ok_or_else(|| invalid_params("limit 必须为 1..200"))? as usize,
    };
    let before = object
        .and_then(|o| o.get("before_id"))
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_i64()
                .filter(|v| *v > 0)
                .ok_or_else(|| invalid_params("before_id 必须为正整数"))
        })
        .transpose()?;
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let ops = tokio::task::spawn_blocking(move || {
        let ops = store.pending_op_page(
            account.as_deref(),
            !include_settled,
            limit + 1,
            before,
            summary,
        )?;
        ops.into_iter()
            .map(|op| {
                let subject = if summary {
                    store.pending_op_subject(op.id)?
                } else {
                    None
                };
                Ok((op, subject))
            })
            .collect::<Result<Vec<_>, rusqlite::Error>>()
    })
    .await
    .map_err(|error| internal(format!("查询任务失败: {error}")))?
    .map_err(|error| internal(format!("查询失败: {error}")))?;
    let more = ops.len() > limit;
    let ops: Vec<Value> = ops
        .iter()
        .take(limit)
        .map(|(op, subject)| {
            if summary {
                pending_op_summary_json(op, subject.as_deref())
            } else {
                pending_op_json(op)
            }
        })
        .collect();
    Ok(json!({ "ops": ops, "has_more": more }))
}

/// `resolvePendingOp`：用户对「失败」或「结果未知」的裁决。
async fn resolve_pending_op(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object
        .keys()
        .any(|key| key != "id" && key != "resolution" && key != "summary_only")
    {
        return Err(invalid_params("参数只能包含 `id` 和 `resolution`"));
    }
    let summary = match object.get("summary_only") {
        None => false,
        Some(Value::Bool(v)) => *v,
        _ => return Err(invalid_params("summary_only 必须为布尔")),
    };
    let id = object
        .get("id")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid_params("`id` 必须是正整数"))?;
    let retry = match object.get("resolution").and_then(Value::as_str) {
        Some("retry") => true,
        Some("discard") => false,
        _ => return Err(invalid_params("`resolution` 必须是 retry 或 discard")),
    };
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let title_store = store.clone();
    let now = unix_now();
    let op = tokio::task::spawn_blocking(move || store.resolve_op(id, retry, now))
        .await
        .map_err(|error| internal(format!("裁决任务失败: {error}")))?
        .map_err(|error| internal(format!("裁决失败: {error}")))?
        .ok_or_else(|| JsonRpcError {
            code: MESSAGE_NOT_FOUND,
            message: "pending op not found".into(),
            data: None,
        })?;
    let subject = if summary {
        title_store
            .pending_op_subject(op.id)
            .map_err(|e| internal(format!("读取邮件摘要失败: {e}")))?
    } else {
        None
    };
    if retry {
        wake_account_writes(ctx, &op.account);
    }
    Ok(
        json!({ "op": if summary {pending_op_summary_json(&op,subject.as_deref())} else {pending_op_json(&op)} }),
    )
}

/// `send`：走 transport 发一封纯文本邮件。params: from / to（必填）/ subject / body。
/// 发信请求允许出现的键。表外键一律拒绝，避免拼错的字段被静默忽略。
const COMPOSE_KEYS: &[&str] = &[
    "account",
    "from",
    "from_address",
    "to",
    "cc",
    "bcc",
    "subject",
    "body",
    "body_html",
    "in_reply_to",
    "references",
    "attachments",
];

/// 收件人字段：接受单个字符串或字符串数组，前者等价于单项数组（ADR-0028 决策 3）。
fn parse_recipients(value: Option<&Value>, field: &str) -> Result<Vec<String>, JsonRpcError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(single)) if !single.is_empty() => Ok(vec![single.clone()]),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let address = item
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid_params(&format!("`{field}` 每项必须是非空字符串")))?;
                out.push(address.to_string());
            }
            Ok(out)
        }
        Some(_) => Err(invalid_params(&format!("`{field}` 必须是字符串或字符串数组"))),
    }
}

/// **逐项**校验地址。校验的是每个 addr-spec，不是拼接后的字符串——
/// 这是头注入防线（ADR-0028 决策 2）。
fn validate_recipients(addresses: &[String], field: &str) -> Result<(), JsonRpcError> {
    for address in addresses {
        maild_client::validate_send_field(maild_client::SendField::To, address)
            .map_err(|error| invalid_params(&format!("`{field}`: {error}")))?;
    }
    Ok(())
}

fn optional_string<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str, JsonRpcError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(invalid_params(&format!("`{key}` 必须是字符串"))),
    }
}

/// 从库里取出待转发的附件（ADR-0028 决策 8：本片不接受新文件上传）。
fn collect_stored_attachments(
    ctx: &Context,
    value: Option<&Value>,
) -> Result<Vec<compose::OutgoingAttachment>, JsonRpcError> {
    let Some(items) = value else {
        return Ok(Vec::new());
    };
    let items = match items {
        Value::Null => return Ok(Vec::new()),
        Value::Array(items) => items,
        _ => return Err(invalid_params("`attachments` 必须是数组")),
    };
    if items.len() > crate::drafts::MAX_DRAFT_ATTACHMENTS {
        return Err(invalid_params("附件数量超过上限"));
    }
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let mut out = Vec::with_capacity(items.len());
    let mut total = 0u64;
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| invalid_params("`attachments` 每项必须是对象"))?;
        if let Some(id) = object.get("upload_id") {
            if object.keys().any(|k| k != "upload_id" && k != "draft_id") {
                return Err(invalid_params("新附件只接受 draft_id/upload_id"));
            }
            let id = id
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| invalid_params("upload_id 无效"))?;
            let draft_id = object
                .get("draft_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| invalid_params("draft_id 无效"))?;
            let file = store
                .draft_file_content(draft_id, id)
                .map_err(|_| internal("附件读取失败".into()))?
                .ok_or_else(|| JsonRpcError {
                    code: ATTACHMENT_NOT_FOUND,
                    message: "draft attachment not found".into(),
                    data: None,
                })?;
            total += file.content.len() as u64;
            if total > crate::uploads::MESSAGE_BYTES {
                return Err(upload_error(crate::uploads::UploadError::Limit(
                    "attachment_total_bytes",
                    crate::uploads::MESSAGE_BYTES,
                )));
            }
            out.push(file);
            continue;
        }
        let account = object
            .get("account")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_params("附件缺少 `account`"))?;
        let folder = object
            .get("folder")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= MAX_FOLDER_NAME_BYTES)
            .ok_or_else(|| invalid_params("附件缺少 `folder`"))?;
        let uid = object
            .get("uid")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_params("附件缺少 `uid`"))?;
        let part_index = object
            .get("part_index")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| invalid_params("附件 part_index 无效"))?;
        let metadata = store
            .list_attachments(account, folder, uid)
            .map_err(|error| internal(format!("附件查询失败: {error}")))?
            .into_iter()
            .find(|attachment| attachment.part_index == part_index)
            .ok_or_else(|| invalid_params("附件不存在"))?;
        let content = store
            .attachment_content(account, folder, uid, part_index)
            .map_err(|error| internal(format!("附件读取失败: {error}")))?
            .ok_or_else(|| invalid_params("附件内容未缓存，无法转发"))?;
        total += content.len() as u64;
        if total > crate::uploads::MESSAGE_BYTES {
            return Err(upload_error(crate::uploads::UploadError::Limit(
                "attachment_total_bytes",
                crate::uploads::MESSAGE_BYTES,
            )));
        }
        out.push(compose::OutgoingAttachment {
            filename: metadata.filename,
            mime_type: metadata.mime_type,
            content,
        });
    }
    Ok(out)
}

/// 从请求体组装一封待发送的邮件。`from` 由调用方决定（`send` 用参数，
/// `sendFromAccount` 用账户/别名地址）。
fn build_outgoing(
    ctx: &Context,
    object: &serde_json::Map<String, Value>,
    from: &str,
) -> Result<compose::OutgoingMessage, JsonRpcError> {
    let to = parse_recipients(object.get("to"), "to")?;
    let cc = parse_recipients(object.get("cc"), "cc")?;
    let bcc = parse_recipients(object.get("bcc"), "bcc")?;
    if to.is_empty() && cc.is_empty() && bcc.is_empty() {
        return Err(invalid_params("至少需要一个收件人"));
    }
    validate_recipients(&to, "to")?;
    validate_recipients(&cc, "cc")?;
    validate_recipients(&bcc, "bcc")?;
    let subject = optional_string(object, "subject")?.to_string();
    let body = optional_string(object, "body")?.to_string();
    let body_html = optional_string(object, "body_html")?.to_string();
    maild_client::validate_send_field(maild_client::SendField::Subject, &subject)
        .map_err(|error| invalid_params(&error.to_string()))?;
    for value in [&body, &body_html] {
        maild_client::validate_send_field(maild_client::SendField::Body, value)
            .map_err(|error| invalid_params(&error.to_string()))?;
    }
    let in_reply_to = compose::sanitize_message_id(optional_string(object, "in_reply_to")?);
    let references = compose::sanitize_references(optional_string(object, "references")?);
    let attachments = collect_stored_attachments(ctx, object.get("attachments"))?;
    let now = unix_now();
    Ok(compose::OutgoingMessage {
        from: from.to_string(),
        to,
        cc,
        bcc,
        subject,
        body,
        body_html,
        in_reply_to,
        references,
        attachments,
        message_id: compose::new_message_id(from, now),
        date_unix: now,
    })
}

/// 投递成功后把同一份报文归档到已发送。失败不影响「已经发出去」这个事实
/// （ADR-0028 决策 9）。
fn queue_sent_append(ctx: &Context, account: &str, raw: &str) -> bool {
    let Some(store) = ctx.get::<Store>() else {
        return false;
    };
    let sent = store
        .list_folders(Some(account))
        .ok()
        .and_then(|folders| {
            folders
                .into_iter()
                .find(|record| record.role == Some(FolderRole::Sent))
        })
        .map(|record| record.name);
    let Some(sent) = sent else { return false };
    match store.queue_append_op(account, &sent, raw, unix_now()) {
        Ok(queued) => {
            if queued {
                wake_account_writes(ctx, account);
            }
            queued
        }
        Err(error) => {
            eprintln!("[maild/api] 归档到已发送入队失败: {error}");
            false
        }
    }
}

async fn send(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    let from = object
        .get("from")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid_params("`from` 必填"))?;
    if object.keys().any(|key| !COMPOSE_KEYS.contains(&key.as_str())) {
        return Err(invalid_params("send 参数无效"));
    }
    maild_client::validate_send_field(maild_client::SendField::From, from)
        .map_err(|error| invalid_params(&error.to_string()))?;
    let message = build_outgoing(ctx, object, from)?;
    let transport = ctx
        .get::<Transport>()
        .ok_or_else(|| unavailable("transport"))?;
    transport
        .send_message(&message)
        .await
        .map_err(|e| internal(format!("发送失败: {e}")))?;
    Ok(json!({ "ok": true, "message_id": message.message_id }))
}

// —— 账户管理方法（M3.7 additive，ADR-0015 §9） ——

fn account_error(error: crate::accounts::ManagerError) -> JsonRpcError {
    use crate::accounts::ManagerError;
    match error {
        ManagerError::NotFound => JsonRpcError {
            code: ACCOUNT_NOT_FOUND,
            message: "account not found".into(),
            data: None,
        },
        ManagerError::Conflict(detail) => JsonRpcError {
            code: ACCOUNT_CONFLICT,
            message: detail,
            data: None,
        },
        ManagerError::ProbeFailed(report) => JsonRpcError {
            code: ACCOUNT_PROBE_FAILED,
            message: "account probe failed".into(),
            data: serde_json::to_value(report).ok(),
        },
        ManagerError::Credential(error) => JsonRpcError {
            code: CREDENTIAL_UNAVAILABLE,
            message: error.to_string(),
            data: None,
        },
        ManagerError::Invalid(detail) => invalid_params(&detail),
        ManagerError::Unavailable(detail) => JsonRpcError {
            code: SERVICE_UNAVAILABLE,
            message: detail,
            data: None,
        },
        // M4-05（ADR-0021）：委托 transport 的失败原样携带 -32018/-32019/-32020。
        ManagerError::Provider(code, detail) => JsonRpcError {
            code,
            message: detail,
            data: None,
        },
        ManagerError::Delivery(error) => internal(format!("发送失败: {}", error.detail)),
        ManagerError::Internal(detail) => internal(detail),
    }
}

fn require_manager(ctx: &Context) -> Result<Arc<crate::accounts::AccountManager>, JsonRpcError> {
    ctx.get::<crate::accounts::AccountManager>()
        .ok_or_else(|| unavailable("account manager（需要平台默认配置启动的 Agent）"))
}

fn ensure_secret_origin(origin: RequestOrigin) -> Result<(), JsonRpcError> {
    if origin.allows_secret_mutations() {
        return Ok(());
    }
    Err(JsonRpcError {
        code: INVALID_REQUEST,
        message: format!(
            "携带秘密的账户方法只接受同用户 Unix socket 传输（当前 {}）",
            origin.as_str()
        ),
        data: None,
    })
}

// —— 插件身份认证（ADR-0017）——

/// `authenticate` 只接受同用户 IPC；stdio/HTTP 会话恒为 `User` 主体。
fn ensure_auth_origin(origin: RequestOrigin) -> Result<(), JsonRpcError> {
    if matches!(
        origin,
        RequestOrigin::UnixSocket | RequestOrigin::NamedPipe
    ) {
        return Ok(());
    }
    Err(JsonRpcError {
        code: AUTH_UNSUPPORTED_ORIGIN,
        message: format!(
            "authenticate 仅支持 Unix socket / named pipe 来源（当前 {}）",
            origin.as_str()
        ),
        data: None,
    })
}

/// 凭证发放（revealPluginToken）允许两种同用户 IPC：插件凭证不经 Keychain，
/// Windows named pipe 无平台提示障碍（与账户秘密的来源门禁不同）。
fn ensure_plugin_secret_origin(origin: RequestOrigin) -> Result<(), JsonRpcError> {
    if matches!(
        origin,
        RequestOrigin::UnixSocket | RequestOrigin::NamedPipe
    ) {
        return Ok(());
    }
    Err(JsonRpcError {
        code: INVALID_REQUEST,
        message: format!(
            "插件凭证发放只接受同用户 Unix socket / named pipe 传输（当前 {}）",
            origin.as_str()
        ),
        data: None,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthenticateParams {
    token: String,
}

/// 绑定会话主体。通知形态不落绑定（无应答通道，调用方也不发送响应）；
/// `-32014` 一律标记 `close_after_reply`，绝不静默降级为 `User`。
fn authenticate_session(
    ctx: &Context,
    origin: RequestOrigin,
    session: &mut SessionAuth,
    params: &Value,
    is_notification: bool,
) -> Result<Value, JsonRpcError> {
    if is_notification {
        return Ok(json!({}));
    }
    ensure_auth_origin(origin)?;
    if session.principal() != Principal::User {
        session.close_after_reply = true;
        eprintln!("[maild/auth] 拒绝重复 authenticate（origin={}）", origin.as_str());
        return Err(JsonRpcError {
            code: AUTH_REJECTED,
            message: "会话已认证，不能重复 authenticate".into(),
            data: None,
        });
    }
    let params: AuthenticateParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("authenticate 参数无效"))?;
    let Some(seam) = ctx.get::<IdentitySeam>() else {
        return Err(JsonRpcError {
            code: AUTH_UNSUPPORTED_ORIGIN,
            message: "插件身份缝未装配".into(),
            data: None,
        });
    };
    match seam.verify(&params.token) {
        Ok(principal) => {
            session.principal.set(principal);
            eprintln!(
                "[maild/auth] 会话绑定主体 {}（origin={}）",
                session.principal().as_str(),
                origin.as_str()
            );
            Ok(json!({}))
        }
        Err(VerifyError::Invalid) => {
            session.close_after_reply = true;
            eprintln!("[maild/auth] 凭证无效或已吊销（origin={}）", origin.as_str());
            Err(JsonRpcError {
                code: AUTH_REJECTED,
                message: "凭证无效或已吊销".into(),
                data: None,
            })
        }
        Err(VerifyError::Unavailable) => {
            eprintln!(
                "[maild/auth] 身份缝停摆，拒绝 authenticate（origin={}）",
                origin.as_str()
            );
            Err(JsonRpcError {
                code: AUTH_UNSUPPORTED_ORIGIN,
                message: "插件身份缝停摆".into(),
                data: None,
            })
        }
    }
}

/// 诊断方法：报告当前会话主体（未认证恒为 `user`）。
async fn whoami_session(session: &SessionAuth, params: &Value) -> HandlerResult {
    require_empty_params(params)?;
    Ok(json!({ "principal": session.principal().as_str() }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevealPluginTokenParams {
    id: String,
    /// 缺省 `plugin`；`frontend` 用于 on_demand 前端包。
    #[serde(default)]
    kind: Option<String>,
}

// —— 插件安装事务（ADR-0019）：全部方法落 ADR-0018 表外默认 = UserOnly ——

fn transaction_error(reason: String) -> JsonRpcError {
    JsonRpcError {
        code: PLUGIN_TRANSACTION_FAILED,
        message: reason,
        data: None,
    }
}

fn install_root_of(ctx: &Context, kind: crate::plugin_transactions::TxKind) -> Result<PathBuf, JsonRpcError> {
    match kind {
        crate::plugin_transactions::TxKind::External => Ok(ctx
            .get::<crate::external::ExternalInstallRoot>()
            .ok_or_else(|| unavailable("external"))?
            .root
            .clone()),
        crate::plugin_transactions::TxKind::Frontend => Ok(ctx
            .get::<crate::frontend::FrontendInstallRoot>()
            .ok_or_else(|| unavailable("frontend"))?
            .0
            .clone()),
    }
}

/// 事务落盘后的生效步骤：external 差异化 refresh；frontend 重扫目录、
/// 热替换快照并对账身份条目。
async fn apply_post_transaction(
    ctx: &Context,
    kind: crate::plugin_transactions::TxKind,
) -> Result<(), JsonRpcError> {
    match kind {
        crate::plugin_transactions::TxKind::External => {
            if let Some(manager) = ctx.get::<crate::external::ExternalPluginManager>() {
                manager.refresh().await.map_err(transaction_error)?;
            }
        }
        crate::plugin_transactions::TxKind::Frontend => {
            if let Some(broker) = ctx.get::<crate::frontend::FrontendBroker>() {
                let root = ctx.get::<crate::frontend::FrontendInstallRoot>().map(|r| r.0.clone());
                broker
                    .reload_from_disk(root.as_deref())
                    .map_err(transaction_error)?;
                if let Some(seam) = ctx.get::<IdentitySeam>() {
                    let ids: Vec<String> = broker
                        .frontend_ids()
                        .into_iter()
                        .filter(|id| !is_builtin_frontend(id))
                        .collect();
                    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
                    let _ = seam.reconcile(PluginKind::Frontend, &refs);
                }
            }
        }
    }
    Ok(())
}

// Package mutation and default selection share one serial publication lane.
static PLUGIN_MUTATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn stop_managed_agent(ctx: &Context, origin: RequestOrigin, params: &Value) -> HandlerResult {
    if !matches!(origin, RequestOrigin::UnixSocket | RequestOrigin::NamedPipe) {
        return Err(invalid_params("后台停止仅接受同用户本地 IPC"));
    }
    require_empty_params(params)?;
    let control = ctx.get::<crate::agent_control::ManagedAgentControl>()
        .ok_or_else(|| unavailable("platform Agent control"))?;
    // Push `agent/stopping` to authenticated frontend streams first, so they spend the drain
    // window flushing drafts and leaving the Dock instead of discovering a dead socket.
    ctx.emit_agent_stopping();
    control.request_stop();
    Ok(json!({"stopping":true}))
}

/// All frontend writers retain filesystem ownership even if their plugin is being unloaded.
/// Roots outlive broker services during LIFO teardown; a root alone never authorizes a write.
fn frontend_mutation_lease(
    ctx: &Context,
    kind: crate::plugin_transactions::TxKind,
) -> Result<Option<Arc<crate::frontend::FrontendDirectoryLease>>, JsonRpcError> {
    if kind == crate::plugin_transactions::TxKind::External {
        return Ok(None);
    }
    let lease = ctx
        .get::<crate::frontend::FrontendDirectoryLease>()
        .ok_or_else(|| unavailable("frontend directory lease"))?;
    let broker = ctx.get::<crate::frontend::FrontendBroker>()
        .ok_or_else(|| unavailable("frontend"))?;
    if !broker.permits_package_mutation() { return Err(transaction_error("前端目录处于恢复状态，未执行包变更".into())); }
    Ok(Some(lease))
}

/// Native launcher only: rerun the startup refresh (newer or same-version-different-build bundled
/// frontend) for an Agent that outlived an app reinstall. Unlike repair, the persisted default
/// selection is left untouched.
async fn resync_builtin_frontend(
    ctx: &Context,
    origin: RequestOrigin,
    params: &Value,
) -> HandlerResult {
    if !matches!(origin, RequestOrigin::UnixSocket | RequestOrigin::NamedPipe) {
        return Err(invalid_params("内置前端同步只接受同用户本地 IPC"));
    }
    require_empty_params(params)?;
    let transaction = PLUGIN_MUTATION.lock().await;
    let source = ctx
        .get::<crate::recovery::BundledFrontend>()
        .ok_or_else(|| unavailable("bundled frontend recovery"))?;
    let root = install_root_of(ctx, crate::plugin_transactions::TxKind::Frontend)?;
    let broker = ctx
        .get::<crate::frontend::FrontendBroker>()
        .ok_or_else(|| unavailable("frontend"))?;
    let directory_lease = ctx
        .get::<crate::frontend::FrontendDirectoryLease>()
        .ok_or_else(|| unavailable("frontend directory lease"))?;
    tokio::task::spawn_blocking(move || {
        let _transaction = transaction;
        let _directory_lease = directory_lease;
        let outcome = match source.refresh(&root) {
            Ok(outcome) => outcome,
            Err(error) => {
                // A failure past the backup rename can leave the package missing on disk; never
                // keep advertising it from the stale in-memory catalog.
                let _ = broker.reload_from_disk(Some(&root));
                return Err(transaction_error(error));
            }
        };
        let replaced = !matches!(
            outcome,
            crate::recovery::RefreshOutcome::Absent | crate::recovery::RefreshOutcome::Kept
        );
        if replaced {
            broker.reload_from_disk(Some(&root)).map_err(|_| {
                transaction_error("前端已同步到磁盘，后台目录刷新失败，请重启后台".into())
            })?;
        }
        Ok(json!({"id": crate::recovery::BUNDLED_FRONTEND_ID, "replaced": replaced}))
    })
    .await
    .map_err(|_| transaction_error("同步执行中断，磁盘状态不确定，请检查后台后再操作".into()))?
}

/// Native maintenance only: no package path supplied by a webview, plugin, HTTP or stdio caller.
async fn repair_builtin_frontend(
    ctx: &Context,
    origin: RequestOrigin,
    params: &Value,
) -> HandlerResult {
    if !matches!(origin, RequestOrigin::UnixSocket | RequestOrigin::NamedPipe) {
        return Err(invalid_params("内置前端修复只接受同用户本地 IPC"));
    }
    require_empty_params(params)?;
    let transaction = PLUGIN_MUTATION.lock().await;
    let source = ctx
        .get::<crate::recovery::BundledFrontend>()
        .ok_or_else(|| unavailable("bundled frontend recovery"))?;
    let root = install_root_of(ctx, crate::plugin_transactions::TxKind::Frontend)?;
    let broker = ctx
        .get::<crate::frontend::FrontendBroker>()
        .ok_or_else(|| unavailable("frontend"))?;
    let identity = ctx.get::<IdentitySeam>();
    let directory_lease = ctx
        .get::<crate::frontend::FrontendDirectoryLease>()
        .ok_or_else(|| unavailable("frontend directory lease"))?;
    // Copy/fsync may take time. Keep the publication lane, but never stall receive/API tasks.
    tokio::task::spawn_blocking(move || {
        // A disconnected client must not release the lane while copying still runs. Move the
        // lease AND snapshot refresh into the uncancellable blocking job.
        let _transaction = transaction;
        let _directory_lease = directory_lease;
        let outcome = source.repair(&root).map_err(transaction_error)?;
        broker.reload_after_repair(&root).map_err(|_| {
            transaction_error("前端已恢复到磁盘，后台目录刷新失败，请重启后台".into())
        })?;
        if let Some(seam) = identity {
            let ids: Vec<String> = broker
                .frontend_ids()
                .into_iter()
                .filter(|id| !is_builtin_frontend(id))
                .collect();
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            seam.reconcile(PluginKind::Frontend, &refs)
                .map_err(|_| transaction_error("前端已恢复，身份对账失败，请重启后台".into()))?;
        }
        serde_json::to_value(outcome).map_err(|_| internal("恢复结果序列化失败".into()))
    })
    .await
    .map_err(|_| transaction_error("修复执行中断，磁盘状态不确定，请检查后台后再操作".into()))?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRootParams { kind: Option<String> }
async fn plugin_install_root(ctx: &Context, params: &Value) -> HandlerResult {
    let p: InstallRootParams = serde_json::from_value(params.clone()).map_err(|_| invalid_params("kind 无效"))?;
    let kind = crate::plugin_transactions::TxKind::parse(p.kind.as_deref()).ok_or_else(|| invalid_params("kind 无效"))?;
    Ok(json!({"root": install_root_of(ctx, kind)?}))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewPackageParams { kind: Option<String>, staging: Option<String>, rollback_id: Option<String> }
async fn preview_plugin_package(ctx: &Context, params: &Value) -> HandlerResult {
    let _transaction = PLUGIN_MUTATION.lock().await;
    let p: PreviewPackageParams = serde_json::from_value(params.clone()).map_err(|_| invalid_params("预览参数无效"))?;
    let kind = crate::plugin_transactions::TxKind::parse(p.kind.as_deref()).ok_or_else(|| invalid_params("kind 无效"))?;
    let info = crate::plugin_transactions::preview(&install_root_of(ctx,kind)?,kind,p.staging.as_deref(),p.rollback_id.as_deref()).map_err(transaction_error)?;
    Ok(json!(info))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultFrontendParams { id: String }
async fn set_default_frontend(ctx: &Context, params: &Value) -> HandlerResult {
    let _transaction = PLUGIN_MUTATION.lock().await;
    let _lease = frontend_mutation_lease(ctx, crate::plugin_transactions::TxKind::Frontend)?;
    let p: DefaultFrontendParams = serde_json::from_value(params.clone()).map_err(|_| invalid_params("前端 id 无效"))?;
    let broker = ctx.get::<crate::frontend::FrontendBroker>().ok_or_else(|| unavailable("frontend"))?;
    broker.set_default(&p.id).map_err(transaction_error)?;
    Ok(json!({"default":broker.default_frontend(),"persisted":true}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitPluginInstallParams {
    /// 缺省 `plugin`；`frontend` 用于前端包。
    #[serde(default)]
    kind: Option<String>,
    staging: String,
    #[serde(default)]
    expected_preview: Option<maild_client::PackagePreview>,
}

async fn commit_plugin_install(ctx: &Context, params: &Value) -> HandlerResult {
    let _transaction = PLUGIN_MUTATION.lock().await;
    let params: CommitPluginInstallParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("commitPluginInstall 参数无效"))?;
    let kind = crate::plugin_transactions::TxKind::parse(params.kind.as_deref())
        .ok_or_else(|| invalid_params("未知 kind"))?;
    let _lease = frontend_mutation_lease(ctx, kind)?;
    let root = install_root_of(ctx, kind)?;
    if let Some(expected) = &params.expected_preview {
        let actual = crate::plugin_transactions::preview(&root, kind, Some(&params.staging), None).map_err(transaction_error)?;
        if &actual != expected { return Err(transaction_error("安装内容已变化，请重新确认".into())); }
    }
    let outcome = crate::plugin_transactions::commit_install(&root, kind, &params.staging)
        .map_err(transaction_error)?;
    apply_post_transaction(ctx, kind).await?;
    eprintln!(
        "[maild/auth] 插件事务完成: {} {}（{}）",
        outcome.action, outcome.id, outcome.version
    );
    serde_json::to_value(outcome).map_err(|_| internal("事务结果序列化失败".into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginIdParams {
    #[serde(default)]
    kind: Option<String>,
    id: String,
    #[serde(default)]
    expected_preview: Option<maild_client::PackagePreview>,
}

async fn remove_plugin(ctx: &Context, params: &Value) -> HandlerResult {
    let _transaction = PLUGIN_MUTATION.lock().await;
    let params: PluginIdParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("removePlugin 参数无效"))?;
    let kind = crate::plugin_transactions::TxKind::parse(params.kind.as_deref())
        .ok_or_else(|| invalid_params("未知 kind"))?;
    let _lease = frontend_mutation_lease(ctx, kind)?;
    let root = install_root_of(ctx, kind)?;
    if kind == crate::plugin_transactions::TxKind::Frontend {
        let broker = ctx.get::<crate::frontend::FrontendBroker>()
            .ok_or_else(|| unavailable("frontend"))?;
        if !broker.can_remove(&params.id) {
            return Err(transaction_error("请先选择其他默认前端；状态不确定时请先恢复后台".into()));
        }
    }
    // Persist disabled intent before moving the package: a crash after removal must not leave
    // a missing enabled id that prevents the next daemon startup. Failed removal stays disabled.
    if kind == crate::plugin_transactions::TxKind::External {
        if let Some(manager) = ctx.get::<crate::external::ExternalPluginManager>() {
            if manager.status(None, None).plugins.iter().any(|p| p.id == params.id && p.enabled) {
                manager.set_enabled(&params.id, false).await.map_err(transaction_error)?;
            }
        }
    }
    crate::plugin_transactions::remove_package(&root, &params.id).map_err(transaction_error)?;
    apply_post_transaction(ctx, kind).await?;
    Ok(json!({ "removed": params.id }))
}

async fn rollback_plugin(ctx: &Context, params: &Value) -> HandlerResult {
    let _transaction = PLUGIN_MUTATION.lock().await;
    let params: PluginIdParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("rollbackPlugin 参数无效"))?;
    let kind = crate::plugin_transactions::TxKind::parse(params.kind.as_deref())
        .ok_or_else(|| invalid_params("未知 kind"))?;
    let _lease = frontend_mutation_lease(ctx, kind)?;
    let root = install_root_of(ctx, kind)?;
    if let Some(expected) = &params.expected_preview {
        let actual = crate::plugin_transactions::preview(&root, kind, None, Some(&params.id)).map_err(transaction_error)?;
        if &actual != expected { return Err(transaction_error("回滚内容已变化，请重新确认".into())); }
    }
    let version = crate::plugin_transactions::rollback_package(&root, kind, &params.id)
        .map_err(transaction_error)?;
    apply_post_transaction(ctx, kind).await?;
    Ok(json!({ "id": params.id, "version": version }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetPluginEnabledParams {
    id: String,
    enabled: bool,
}

async fn set_plugin_enabled(ctx: &Context, params: &Value) -> HandlerResult {
    let _transaction = PLUGIN_MUTATION.lock().await;
    let params: SetPluginEnabledParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("setPluginEnabled 参数无效"))?;
    let manager = ctx
        .get::<crate::external::ExternalPluginManager>()
        .ok_or_else(|| unavailable("external"))?;
    manager
        .set_enabled(&params.id, params.enabled)
        .await
        .map_err(transaction_error)?;
    Ok(json!({ "id": params.id, "enabled": params.enabled }))
}

async fn refresh_plugins(ctx: &Context, params: &Value) -> HandlerResult {
    let _transaction = PLUGIN_MUTATION.lock().await;
    require_empty_params(params)?;
    let mut external = json!({});
    if let Some(manager) = ctx.get::<crate::external::ExternalPluginManager>() {
        let report = manager.refresh().await.map_err(transaction_error)?;
        external = serde_json::to_value(report).unwrap_or(json!({}));
    }
    let mut frontend = json!({});
    if let Some(broker) = ctx.get::<crate::frontend::FrontendBroker>() {
        let root = ctx.get::<crate::frontend::FrontendInstallRoot>().map(|r| r.0.clone());
        match broker.reload_from_disk(root.as_deref()) {
            Ok(()) => {
                if let Some(seam) = ctx.get::<IdentitySeam>() {
                    let ids: Vec<String> = broker
                        .frontend_ids()
                        .into_iter()
                        .filter(|id| !is_builtin_frontend(id))
                        .collect();
                    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
                    let _ = seam.reconcile(PluginKind::Frontend, &refs);
                }
                frontend = json!({ "reloaded": true });
            }
            Err(reason) => frontend = json!({ "reloaded": false, "detail": reason }),
        }
    }
    Ok(json!({ "external": external, "frontend": frontend }))
}

/// reveal 即轮换：每次生成新凭证、旧凭证对新连接失效，明文只在本次应答出现。
async fn reveal_plugin_token(
    ctx: &Context,
    origin: RequestOrigin,
    params: &Value,
) -> Result<Value, JsonRpcError> {
    ensure_plugin_secret_origin(origin)?;
    let params: RevealPluginTokenParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("revealPluginToken 参数无效"))?;
    let kind = match params.kind.as_deref() {
        None | Some("plugin") => PluginKind::External,
        Some("frontend") => PluginKind::Frontend,
        Some(other) => return Err(invalid_params(&format!("未知 kind: {other}"))),
    };
    let seam = ctx.get::<IdentitySeam>().ok_or_else(|| JsonRpcError {
        code: AUTH_UNSUPPORTED_ORIGIN,
        message: "插件身份缝未装配".into(),
        data: None,
    })?;
    let token = seam.reveal(kind, &params.id).map_err(|error| match error {
        RotateError::UnknownId => invalid_params("插件未安装或没有身份条目"),
        RotateError::Unavailable => JsonRpcError {
            code: AUTH_UNSUPPORTED_ORIGIN,
            message: "插件身份缝停摆".into(),
            data: None,
        },
    })?;
    eprintln!(
        "[maild/auth] 凭证已轮换并发放 {}:{}（origin={}）",
        kind.as_str(),
        params.id,
        origin.as_str()
    );
    Ok(json!({ "token": token }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListAccountProfilesParams {
    #[serde(default)]
    include_archived: Option<bool>,
}

async fn list_account_profiles(ctx: &Context, params: &Value) -> HandlerResult {
    let params: ListAccountProfilesParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("参数只允许 include_archived 布尔值"))?;
    let manager = require_manager(ctx)?;
    Ok(json!({
        "profiles": manager.profiles_view(params.include_archived.unwrap_or(false)),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeAccountParams {
    draft: crate::accounts::AccountDraft,
    credentials: crate::accounts::AccountCredentials,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    expected_revision: Option<u64>,
}

async fn probe_account_rpc(ctx: &Context, origin: RequestOrigin, params: &Value) -> HandlerResult {
    ensure_secret_origin(origin)?;
    let params: ProbeAccountParams =
        serde_json::from_value(params.clone()).map_err(|_| invalid_params("draft/credentials 参数无效"))?;
    let manager = require_manager(ctx)?;
    let report = match (params.id.as_deref(), params.expected_revision) {
        (None, None) => manager.probe_account(&params.draft, &params.credentials).await,
        (Some(id), Some(revision)) if !id.is_empty() && revision > 0 =>
            manager.probe_existing_account(id, revision, &params.draft, &params.credentials).await,
        _ => return Err(invalid_params("已有账户探测须同时提供 id 和 expected_revision")),
    }.map_err(account_error)?;
    serde_json::to_value(report).map_err(|_| internal("探测结果序列化失败".into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateAccountParams {
    draft: crate::accounts::AccountDraft,
    credentials: crate::accounts::AccountCredentials,
}

async fn create_account_rpc(
    ctx: &Context,
    origin: RequestOrigin,
    params: &Value,
) -> HandlerResult {
    ensure_secret_origin(origin)?;
    let params: CreateAccountParams =
        serde_json::from_value(params.clone()).map_err(|_| invalid_params("draft/credentials 参数无效"))?;
    let manager = require_manager(ctx)?;
    let (profile, active) = manager
        .create_account(params.draft, params.credentials)
        .await
        .map_err(account_error)?;
    let view = manager
        .profiles_view(true)
        .into_iter()
        .find(|view| view.id == profile.id)
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| internal("profile 序列化失败".into()))?;
    Ok(json!({ "profile": view, "persisted": true, "active": active }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateAccountParams {
    id: String,
    expected_revision: u64,
    patch: crate::accounts::AccountPatch,
    #[serde(default)]
    credentials: Option<crate::accounts::AccountCredentials>,
}

async fn update_account_rpc(
    ctx: &Context,
    origin: RequestOrigin,
    params: &Value,
) -> HandlerResult {
    let params: UpdateAccountParams =
        serde_json::from_value(params.clone()).map_err(|_| invalid_params("update 参数无效"))?;
    // 只有携带新秘密的更新受传输限制；纯资料编辑走任意本地传输。
    if params.credentials.is_some() {
        ensure_secret_origin(origin)?;
    }
    let manager = require_manager(ctx)?;
    let view = manager
        .update_account(
            &params.id,
            params.expected_revision,
            params.patch,
            params.credentials,
        )
        .await
        .map_err(account_error)?;
    serde_json::to_value(view).map_err(|_| internal("profile 序列化失败".into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetAccountEnabledParams {
    id: String,
    expected_revision: u64,
    enabled: bool,
}

async fn set_account_enabled_rpc(ctx: &Context, params: &Value) -> HandlerResult {
    let params: SetAccountEnabledParams =
        serde_json::from_value(params.clone()).map_err(|_| invalid_params("enabled 参数无效"))?;
    let manager = require_manager(ctx)?;
    let view = manager
        .set_account_enabled(&params.id, params.expected_revision, params.enabled)
        .await
        .map_err(account_error)?;
    serde_json::to_value(view).map_err(|_| internal("profile 序列化失败".into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveAccountParams {
    id: String,
    expected_revision: u64,
}

async fn remove_account_rpc(ctx: &Context, params: &Value) -> HandlerResult {
    let params: RemoveAccountParams =
        serde_json::from_value(params.clone()).map_err(|_| invalid_params("remove 参数无效"))?;
    let manager = require_manager(ctx)?;
    let outcome = manager
        .remove_account(&params.id, params.expected_revision)
        .await
        .map_err(account_error)?;
    serde_json::to_value(outcome).map_err(|_| internal("remove 结果序列化失败".into()))
}


async fn send_from_account_rpc(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object.keys().any(|key| !COMPOSE_KEYS.contains(&key.as_str())) {
        return Err(invalid_params("sendFromAccount 参数无效"));
    }
    let account = object
        .get("account")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_params("`account` 必填"))?
        .to_owned();
    let from_address = match object.get("from_address") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(_) => return Err(invalid_params("`from_address` 必须是非空字符串或 null")),
    };
    // From 由 manager 解析（别名校验），这里只用 account 占位生成一次草稿。
    let draft = build_outgoing(ctx, object, "draft@invalid")?;
    let manager = require_manager(ctx)?;
    let raw = manager
        .send_message_from_account(&account, &draft, from_address.as_deref())
        .await
        .map_err(account_error)?;
    let archived = queue_sent_append(ctx, &account, &raw);
    Ok(json!({ "ok": true, "archived": archived }))
}

struct DraftDeliveryGuard {
    store: std::sync::Weak<Store>,
    id: String,
    version: u64,
    settled: bool,
}
impl Drop for DraftDeliveryGuard {
    fn drop(&mut self) {
        if !self.settled {
            if let Some(store) = self.store.upgrade() {
                let _ =
                    store.settle_draft_send(&self.id, self.version, "unknown", None, unix_now());
            }
        }
    }
}

async fn send_draft(ctx: &Context, params: &Value, resolve: bool) -> HandlerResult {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        id: String,
        base_version: u64,
    }
    let input: Input = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("需要 id/base_version"))?;
    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    if resolve {
        return match store
            .resolve_draft_send(&input.id, input.base_version, unix_now())
            .map_err(|_| internal("草稿写入失败".into()))?
        {
            DraftWrite::Saved(draft) => Ok(json!({"draft":draft})),
            other => Err(draft_write_error(other)),
        };
    }
    let manager = require_manager(ctx)?;
    let current = store
        .get_draft(&input.id)
        .map_err(|_| internal("读取草稿失败".into()))?
        .ok_or_else(draft_not_found)?;
    if current.version != input.base_version {
        return Err(draft_write_error(DraftWrite::Conflict {
            version: current.version,
        }));
    }
    if !matches!(current.send_state.as_str(), "editing" | "failed") {
        return Err(draft_write_error(DraftWrite::SendLocked));
    }
    let attachments: Vec<Value> = current
        .attachments
        .iter()
        .map(|a| match &a.upload_id {
            Some(id) => json!({"draft_id":current.id,"upload_id":id}),
            None => {
                json!({"account":a.account,"folder":a.folder,"uid":a.uid,"part_index":a.part_index})
            }
        })
        .collect();
    let fields = json!({"to":current.to,"cc":current.cc,"bcc":current.bcc,"subject":current.subject,"body":current.body,
        "body_html":current.body_html,"in_reply_to":current.in_reply_to,"references":current.references,"attachments":attachments});
    let outgoing = build_outgoing(ctx, fields.as_object().unwrap(), "draft@invalid")?;
    let locked = match store
        .claim_draft_send(&input.id, input.base_version, unix_now())
        .map_err(|_| internal("锁定草稿失败".into()))?
    {
        DraftWrite::Saved(d) => d,
        other => return Err(draft_write_error(other)),
    };
    let mut guard = DraftDeliveryGuard {
        store: Arc::downgrade(&store),
        id: locked.id.clone(),
        version: locked.version,
        settled: false,
    };
    drop(store); // The delivery fence must not pin the database lock across network waits.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        manager.send_message_from_account(
            &locked.account,
            &outgoing,
            (!locked.from.is_empty()).then_some(locked.from.as_str()),
        ),
    )
    .await;
    use crate::accounts::ManagerError;
    let state = match &result {
        Ok(Ok(_)) => "sent",
        Ok(Err(ManagerError::Delivery(e))) if !e.uncertain => "failed",
        Ok(Err(
            ManagerError::Invalid(_)
            | ManagerError::NotFound
            | ManagerError::Conflict(_)
            | ManagerError::Credential(_)
            | ManagerError::Unavailable(_),
        )) => "failed",
        _ => "unknown",
    };
    let store = guard.store.upgrade().ok_or_else(|| unavailable("store"))?;
    let mut saved = match store
        .settle_draft_send(&locked.id, locked.version, state, None, unix_now())
        .map_err(|_| internal("保存投递结果失败，请核对已发送".into()))?
    {
        DraftWrite::Saved(d) => d,
        other => return Err(draft_write_error(other)),
    };
    guard.settled = true;
    if let Ok(Ok(raw)) = result {
        let queued = queue_sent_append(ctx, &locked.account, &raw);
        // Delivery already committed above. A failure to store this secondary fact cannot invite resending.
        if store.record_draft_archive(&locked.id, queued).is_ok() {
            saved.sent_archive = Some(queued);
        }
    }
    Ok(json!({"draft":saved}))
}

/// `prepareReply`：回复 / 全部回复 / 转发的草稿在内核成型（ADR-0028 决策 7）。
async fn prepare_reply(ctx: &Context, params: &Value) -> HandlerResult {
    let object = params
        .as_object()
        .ok_or_else(|| invalid_params("参数必须是对象"))?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "account" | "folder" | "uid" | "mode" | "locale"))
    {
        return Err(invalid_params(
            "参数只能包含 `account`、`folder`、`uid`、`mode` 和 `locale`",
        ));
    }
    let account = object
        .get("account")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_params("`account` 必须是非空字符串"))?
        .to_owned();
    let uid = object
        .get("uid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_params("`uid` 必须是非空字符串"))?
        .to_owned();
    let folder = match object.get("folder") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if !value.is_empty() && value.len() <= MAX_FOLDER_NAME_BYTES => {
            Some(value.clone())
        }
        Some(_) => return Err(invalid_params("`folder` 必须是非空字符串或 null")),
    };
    let mode = object
        .get("mode")
        .and_then(Value::as_str)
        .and_then(compose::DraftMode::parse)
        .ok_or_else(|| invalid_params("`mode` 必须是 reply / reply_all / forward"))?;
    // ADR-0048 决策 7：引用块是邮件内容的语言，可与界面语言不同。给了 `locale` 就只用它协商，
    // 否则用偏好的 `effective` 列表；协商不出结果回落中文（即今天的行为）。
    let locale = match object.get("locale") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(
            crate::locale_preference::normalize_tag(value)
                .map_err(|error| invalid_params(&format!("`locale` 无效：{error}")))?,
        ),
        Some(_) => return Err(invalid_params("`locale` 必须是 BCP 47 字符串或 null")),
    };
    let quote = &match locale {
        Some(tag) => maild_l10n::negotiate(&[tag]),
        None => maild_l10n::negotiate(&effective_locales(ctx)),
    }
    .messages()
    .quote;

    let store = ctx.get::<Store>().ok_or_else(|| unavailable("store"))?;
    let lookup = (account.clone(), folder.clone(), uid.clone());
    let original = tokio::task::spawn_blocking(move || {
        store.get_message(&lookup.0, lookup.1.as_deref(), &lookup.2)
    })
    .await
    .map_err(|error| internal(format!("查询任务失败: {error}")))?
    .map_err(|error| internal(format!("查询失败: {error}")))?
    .ok_or_else(|| JsonRpcError {
        code: MESSAGE_NOT_FOUND,
        message: "message not found".into(),
        data: None,
    })?;

    // 没有账户管理器时（纯 store 装配）拿不到别名集合，全部回复可能带上自己。
    let mine = ctx
        .get::<crate::accounts::AccountManager>()
        .map(|manager| manager.own_addresses(&account))
        .unwrap_or_default();

    let date = compose::attribution_date(quote, original.date_at.unwrap_or(original.received_at));
    let sender_addresses = compose::extract_addresses(&original.sender);
    let draft = match mode {
        compose::DraftMode::Reply | compose::DraftMode::ReplyAll => {
            let reply_to = compose::extract_addresses(&original.reply_to);
            let to = if reply_to.is_empty() {
                sender_addresses.clone()
            } else {
                reply_to
            };
            let cc = if mode == compose::DraftMode::ReplyAll {
                let mut everyone = compose::extract_addresses(&original.recipient);
                everyone.extend(compose::extract_addresses(&original.cc));
                let without_recipients = compose::without_self(everyone, &mine);
                // 已经在 To 里的人不重复抄送。
                without_recipients
                    .into_iter()
                    .filter(|address| {
                        !to.iter().any(|existing| existing.eq_ignore_ascii_case(address))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            json!({
                "to": compose::without_self(to, &mine),
                "cc": cc,
                "subject": compose::reply_subject(&original.subject),
                "body": compose::quote_reply(quote, &original.sender, &date, &original.body),
                "in_reply_to": original.message_id,
                "references": compose::inherited_references(
                    &original.references,
                    &original.in_reply_to,
                    &original.message_id,
                ),
                "attachments": [],
            })
        }
        compose::DraftMode::Forward => {
            let attachments: Vec<Value> = original
                .attachments
                .iter()
                .filter(|attachment| !attachment.inline)
                .map(|attachment| {
                    json!({
                        "account": account,
                        "folder": original.folder,
                        "uid": original.uid,
                        "part_index": attachment.part_index,
                        "filename": attachment.filename,
                        "size": attachment.size,
                    })
                })
                .collect();
            json!({
                "to": [],
                "cc": [],
                "subject": compose::forward_subject(&original.subject),
                "body": compose::quote_forward(
                    quote,
                    &original.sender,
                    &date,
                    &original.subject,
                    &original.recipient,
                    &original.body,
                ),
                // 转发开启的是新会话，不继承 In-Reply-To/References。
                "in_reply_to": "",
                "references": "",
                "attachments": attachments,
            })
        }
    };
    Ok(json!({ "draft": draft }))
}

// —— 地址族 RPC（M4-06，ADR-0022；全部 UserOnly） ——

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListAddressesParams {
    #[serde(default)]
    account: Option<String>,
    /// false = 仅 active（含惰性过期过滤后的存活项）。
    #[serde(default)]
    include_inactive: bool,
    /// 可选有界页；缺省维持 ADR-0022 的全量响应兼容。
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    offset: Option<u64>,
}

async fn list_addresses_rpc(ctx: &Context, params: &Value) -> HandlerResult {
    let params: ListAddressesParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("listAddresses 参数无效"))?;
    let paged = params.limit.is_some() || params.offset.is_some();
    let limit = match params.limit {
        None if paged => 500,
        None => usize::MAX,
        Some(value) if (1..=1000).contains(&value) => value as usize,
        Some(_) => return Err(invalid_params("limit 必须是 1 到 1000 的整数")),
    };
    let offset = usize::try_from(params.offset.unwrap_or(0))
        .ok()
        .filter(|value| *value <= i64::MAX as usize)
        .ok_or_else(|| invalid_params("offset 过大"))?;
    let manager = require_manager(ctx)?;
    let account = params.account;
    let include_inactive = params.include_inactive;
    let mut views = tokio::task::spawn_blocking(move || {
        manager.list_addresses(account.as_deref(), include_inactive)
    })
    .await
    .map_err(|error| internal(format!("地址查询任务失败: {error}")))?
        .map_err(account_error)?;
    let total = views.len();
    if paged {
        views = views.into_iter().skip(offset).take(limit).collect();
    }
    Ok(json!({ "addresses": views, "total": total }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateAddressParams {
    account: String,
    /// "alias" | "disposable"。
    kind: String,
    /// alias 必填；disposable 可省略（daemon 随机生成）。
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    label: Option<String>,
    /// disposable TTL（秒）；缺省永久。
    #[serde(default)]
    expires_secs: Option<u64>,
}

async fn create_address_rpc(ctx: &Context, params: &Value) -> HandlerResult {
    let params: CreateAddressParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("createAddress 参数无效"))?;
    let manager = require_manager(ctx)?;
    let view = manager
        .create_address(
            &params.account,
            &params.kind,
            params.address.as_deref(),
            params.label.as_deref(),
            params.expires_secs,
        )
        .map_err(account_error)?;
    serde_json::to_value(view).map_err(|_| internal("地址视图序列化失败".into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateAddressParams {
    id: String,
    /// "active" | "disabled"（expired 为派生态，不可显式设置）。
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

async fn update_address_rpc(ctx: &Context, params: &Value) -> HandlerResult {
    let params: UpdateAddressParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("updateAddress 参数无效"))?;
    if params.state.is_none() && params.label.is_none() {
        return Err(invalid_params("updateAddress 至少提供 state 或 label"));
    }
    let manager = require_manager(ctx)?;
    let view = manager
        .update_address(&params.id, params.state.as_deref(), params.label.as_deref())
        .map_err(account_error)?;
    serde_json::to_value(view).map_err(|_| internal("地址视图序列化失败".into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveAddressParams {
    id: String,
}

async fn remove_address_rpc(ctx: &Context, params: &Value) -> HandlerResult {
    let params: RemoveAddressParams = serde_json::from_value(params.clone())
        .map_err(|_| invalid_params("removeAddress 参数无效"))?;
    let manager = require_manager(ctx)?;
    manager.remove_address(&params.id).map_err(account_error)?;
    Ok(json!({ "ok": true }))
}

/// `providerRegister`（M4-05，ADR-0021）：把当前连接登记为该插件 id 的
/// 活跃 transport provider 端点。latest-wins；连接关闭自动注销。
async fn provider_register(
    ctx: &Context,
    origin: RequestOrigin,
    session: &mut SessionAuth,
    params: &Value,
) -> HandlerResult {
    match params {
        Value::Null => {}
        Value::Object(object) if object.is_empty() => {}
        _ => return Err(invalid_params("providerRegister 不接受参数")),
    }
    // 策略表放行了 User 主体（Capability 只约束插件主体）；注册本身是
    // 插件动作，这里显式收口。
    let plugin_id = match session.principal() {
        Principal::Plugin(id) => id,
        _ => {
            return Err(JsonRpcError {
                code: INVALID_REQUEST,
                message: "providerRegister 仅限已认证的插件主体".into(),
                data: None,
            })
        }
    };
    let registry = ctx.get::<crate::provider::ProviderRegistry>().ok_or_else(|| JsonRpcError {
        code: crate::provider::PROVIDER_UNAVAILABLE,
        message: "provider 注册表未装配".into(),
        data: None,
    })?;
    let output = session.outbound_handle().ok_or_else(|| internal("会话输出通道缺失".into()))?;

    let endpoint = Arc::new(crate::provider::ProviderEndpoint::new(
        plugin_id.clone(),
        output,
        session.provider_calls(),
    ));
    if let Some(previous) = registry.register(endpoint.clone()) {
        previous.calls().fail_all("provider 已被同一插件的另一连接顶替");
        eprintln!(
            "[maild/provider] 顶替 {} 的旧 provider 端点",
            previous.plugin_id()
        );
    }
    // 同一连接重复注册：先解除自己的旧属主，避免留下悬空登记。
    if let Some((old_id, old_endpoint)) = session.set_registered(plugin_id.clone(), endpoint) {
        let _ = registry.remove_if_owner(&old_id, &old_endpoint);
    }
    eprintln!(
        "[maild/provider] {} 注册为 transport provider（{}）",
        plugin_id,
        origin.as_str()
    );
    Ok(json!({}))
}

/// `getSettings`：读取 rules 运行时配置（不含账户凭据等秘密）。
async fn get_settings(ctx: &Context) -> HandlerResult {
    let settings = ctx
        .get::<RulesSettings>()
        .ok_or_else(|| unavailable("rules"))?;
    Ok(json!({ "rules": settings.snapshot(), "persisted": settings.is_persisted() }))
}

fn rules_patch_error(error: crate::rules::RulesPatchError) -> JsonRpcError {
    use crate::rules::RulesPatchError;
    match error {
        RulesPatchError::Invalid(detail) => invalid_params(&format!("rules 设置无效: {detail}")),
        RulesPatchError::Unavailable(detail) => JsonRpcError {
            code: SERVICE_UNAVAILABLE,
            message: format!("rules 持久化停摆: {detail}"),
            data: None,
        },
        RulesPatchError::Io(detail) => internal(format!("rules 持久化写入失败: {detail}")),
    }
}

/// `setSettings`：合并 rules 部分更新（ADR-0043）。
///
/// `persisted` 如实报告这次写入是否已原子落盘到 `rules_state.toml`：TOML 未开 `persist`
/// 时与 ADR-0006 一致只改内存（`false`）。可选 `persist: true` 在守护进程有受管配置目录
/// 时运行时挂载侧车；没有可用路径时 `-32001`，不静默降级。
async fn set_settings(ctx: &Context, params: &Value) -> HandlerResult {
    let settings = ctx
        .get::<RulesSettings>()
        .ok_or_else(|| unavailable("rules"))?;
    let object = params
        .as_object()
        .filter(|object| {
            object.contains_key("rules")
                && object.keys().all(|key| key == "rules" || key == "persist")
        })
        .ok_or_else(|| invalid_params("参数必须包含 `rules` 对象，可选 `persist`"))?;
    let rules = object
        .get("rules")
        .ok_or_else(|| invalid_params("`rules` 必须是对象"))?;
    let persist = match object.get("persist") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(invalid_params("`persist` 必须是布尔")),
    };
    let patch: RulesPatch = serde_json::from_value(rules.clone())
        .map_err(|error| invalid_params(&format!("rules 设置无效: {error}")))?;
    if persist && !settings.is_persisted() {
        let path = ctx
            .get::<crate::rules::RulesStatePath>()
            .map(|path| path.0.clone())
            .ok_or_else(|| JsonRpcError {
                code: SERVICE_UNAVAILABLE,
                message: "rules 持久化不可用：守护进程没有受管配置目录".into(),
                data: None,
            })?;
        settings
            .enable_persistence(path)
            .map_err(rules_patch_error)?;
    }
    let next = settings.try_patch(&patch).map_err(rules_patch_error)?;
    Ok(json!({ "rules": next, "persisted": settings.is_persisted() }))
}

// —— stdio 传输 ——

/// 服务端推送：事件 → JSON-RPC 通知行。
fn notification(method: &str, params: Value) -> String {
    serde_json::to_string(&JsonRpcNotification {
        jsonrpc: "2.0",
        method: method.into(),
        params,
    })
    .unwrap_or_else(|_| "{}".into())
}

fn bounded_response(response: JsonRpcResponse) -> JsonRpcResponse {
    let id = response.id.clone();
    if serde_json::to_vec(&response).is_ok_and(|line| line.len() <= MAX_RESPONSE_BYTES) {
        return response;
    }
    let fallback = JsonRpcResponse::err(
        id,
        internal(format!(
            "response too large (max {MAX_RESPONSE_BYTES} bytes); reduce page size"
        )),
    );
    if serde_json::to_vec(&fallback).is_ok_and(|line| line.len() <= MAX_RESPONSE_BYTES) {
        fallback
    } else {
        JsonRpcResponse::err(
            Value::Null,
            internal(format!(
                "response too large (max {MAX_RESPONSE_BYTES} bytes); reduce page size"
            )),
        )
    }
}

fn response_line(response: JsonRpcResponse) -> String {
    bounded_response(response).to_line()
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn mail_notification(mut msg: MailMessage) -> String {
    truncate_utf8(&mut msg.account, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.uid, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.sender, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.recipient, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.subject, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.body, MAX_PUSH_BODY_BYTES);
    truncate_utf8(&mut msg.snippet, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.cc, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.bcc, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.reply_to, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.message_id, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.in_reply_to, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut msg.references, MAX_PUSH_TEXT_BYTES);
    // 通知是「有新邮件」的信号，不是持久队列（协议文档已如此声明）：
    // HTML 原文与附件一律不进推送帧（ADR-0025 决策 6）。
    msg.body_html.clear();
    msg.attachments.clear();
    notification(
        "message/received",
        serde_json::to_value(msg).unwrap_or(Value::Null),
    )
}

fn account_notification(mut info: AccountStateInfo) -> String {
    truncate_utf8(&mut info.account, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut info.detail, MAX_PUSH_TEXT_BYTES);
    truncate_utf8(&mut info.server_detail, MAX_PUSH_TEXT_BYTES);
    notification(
        "account/state",
        serde_json::to_value(info).unwrap_or(Value::Null),
    )
}

/// ADR-0045：`frontend/intent` 只投给主体**恰好**是 `frontend:<target>` 的会话——user、插件与
/// 其他前端一律收不到。帧里只有 frontend id；意图本身仍须 `claimFrontendIntent` 一次性领取。
/// `agent/stopping` 只推给已认证前端：user、插件与未认证连接没有需要刷盘的窗口状态。
fn agent_stopping_notification(principal: &Principal) -> Option<String> {
    matches!(principal, Principal::Frontend(_)).then(|| notification("agent/stopping", json!({})))
}

/// 长连接会话对 [`crate::events::AgentStoppingEvent`] 的订阅：按会话当前主体过滤后入写队列。
fn agent_stopping_listener(
    ctx: &Context,
    tx: mpsc::Sender<String>,
    principal: SharedPrincipal,
) -> ListenerGuard {
    ListenerGuard::new(ctx.on_agent_stopping(move |_| {
        if let Some(line) = agent_stopping_notification(&principal.get()) {
            let _ = tx.try_send(line);
        }
    }))
}

/// ADR-0048：`preferences/locale` 推给所有流式连接（`getLocalePreference` 是 `Open`）。
fn locale_notification(effective: Vec<String>) -> String {
    notification("preferences/locale", json!({ "effective": effective }))
}

/// `preferences/proxy` 推给所有流式连接（不含密码的摘要 + `generation`）。
fn proxy_notification(event: ProxyPreferenceEvent) -> String {
    notification(
        "preferences/proxy",
        serde_json::to_value(event).expect("ProxyPreferenceEvent is serializable"),
    )
}

fn frontend_intent_notification(principal: &Principal, target: &str) -> Option<String> {
    match principal {
        Principal::Frontend(id) if id == target => {
            Some(notification("frontend/intent", json!({ "frontend": target })))
        }
        _ => None,
    }
}

async fn read_frame<R>(reader: &mut R) -> io::Result<Option<Result<Vec<u8>, ()>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Ok(Some(Ok(frame)))
            };
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let consumed = newline.map(|i| i + 1).unwrap_or(available.len());
        if frame.len().saturating_add(consumed) > MAX_FRAME_BYTES {
            reader.consume(consumed);
            return Ok(Some(Err(())));
        }
        frame.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            while matches!(frame.last(), Some(b'\n' | b'\r')) {
                frame.pop();
            }
            return Ok(Some(Ok(frame)));
        }
    }
}

/// `serve` 会话的同步事件订阅，正常返回或任务取消时都立即注销。
struct ListenerGuard(Option<Disposer>);

impl ListenerGuard {
    fn new(disposer: Disposer) -> Self {
        ListenerGuard(Some(disposer))
    }
}

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(Disposer::Sync(dispose)) => dispose(),
            Some(Disposer::Async(_)) => unreachable!("事件监听器应返回同步 disposer"),
            None => {}
        }
    }
}

/// A writer may remain blocked after its reader closes. Dropping a bare
/// `JoinHandle` detaches it, so bound the join and make cancellation explicit.
struct WriterTaskGuard(Option<tokio::task::JoinHandle<()>>);

impl WriterTaskGuard {
    fn new(task: tokio::task::JoinHandle<()>) -> Self {
        Self(Some(task))
    }

    async fn join(mut self) {
        let Some(mut task) = self.0.take() else {
            return;
        };
        if tokio::time::timeout(Duration::from_secs(1), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for WriterTaskGuard {
    fn drop(&mut self) {
        if let Some(task) = self.0.as_ref() {
            task.abort();
        }
    }
}

/// JSON-RPC over 行分隔流：读请求 → 分发 → 写响应；事件 → 写通知。
///
/// 输出（响应 + 推送）统一经 mpsc 汇入单一写端任务，避免响应与推送交错。
/// 推送监听器是同步的（emit 同步）无法 await 写，故 `try_send` 满则丢弃（背压）。
async fn serve_with_shutdown<R, W, F>(
    ctx: Context,
    origin: RequestOrigin,
    reader: R,
    writer: W,
    shutdown: F,
) where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<String>(OUTPUT_QUEUE_CAPACITY);

    // 会话状态先行：推送监听器需要共享主体句柄做投递前过滤（ADR-0018）。
    let mut session = SessionAuth::new(origin);
    // 反向请求帧与响应/通知共用同一写任务（ADR-0021）。
    session.attach_output(tx.clone());

    // 订阅事件 → 服务端推送（scope=ROOT：见所有账户）；按主体 capability 过滤。
    let message_listener = {
        let tx = tx.clone();
        let filter_ctx = ctx.clone();
        let principal = session.shared_principal();
        ListenerGuard::new(ctx.on_emit::<MailFiltered>(move |msg| {
            if !principal_has_capability(&filter_ctx, &principal.get(), EVENT_CAPABILITY) {
                return;
            }
            let line = mail_notification(msg);
            let _ = tx.try_send(line);
        }))
    };
    let account_listener = {
        let tx = tx.clone();
        let filter_ctx = ctx.clone();
        let principal = session.shared_principal();
        ListenerGuard::new(ctx.on_emit::<AccountState>(move |info| {
            if !principal_has_capability(&filter_ctx, &principal.get(), EVENT_CAPABILITY) {
                return;
            }
            let line = account_notification(info);
            let _ = tx.try_send(line);
        }))
    };
    // ADR-0045：意图入队 → 唤醒目标前端自己的长连接（隐藏在菜单栏的窗口收不到 Reopen）。
    let intent_listener = {
        let tx = tx.clone();
        let principal = session.shared_principal();
        ListenerGuard::new(ctx.on_emit::<FrontendIntentQueued>(move |frontend| {
            if let Some(line) = frontend_intent_notification(&principal.get(), &frontend) {
                let _ = tx.try_send(line);
            }
        }))
    };
    // ADR-0048：语言偏好变化 → 所有流式连接（不按主体过滤，偏好本身对任何主体开放）。
    let locale_listener = {
        let tx = tx.clone();
        ListenerGuard::new(ctx.on_emit::<LocalePreferenceChanged>(move |effective| {
            let _ = tx.try_send(locale_notification(effective));
        }))
    };
    // 全局代理偏好/系统代理变化 → 所有流式连接（非秘密摘要 + generation）。
    let proxy_listener = {
        let tx = tx.clone();
        ListenerGuard::new(ctx.on_emit::<ProxyPreferenceChanged>(move |event| {
            let _ = tx.try_send(proxy_notification(event));
        }))
    };
    // 用户请求整体停机 → 已认证前端在排空窗口内刷盘草稿、撤下 Dock 图标后自行退出。
    let stopping_listener = agent_stopping_listener(&ctx, tx.clone(), session.shared_principal());

    // 写端任务：独占 writer，逐行输出。
    let (writer_done_tx, mut writer_done_rx) = oneshot::channel();
    let writer_task = WriterTaskGuard::new(tokio::spawn(async move {
        let mut w = writer;
        while let Some(line) = rx.recv().await {
            if w.write_all(line.as_bytes()).await.is_err()
                || w.write_all(b"\n").await.is_err()
                || w.flush().await.is_err()
            {
                break;
            }
        }
        let _ = writer_done_tx.send(());
    }));

    // 读端：逐行解析 → 分发 → 响应。会话状态（ADR-0017 主体绑定）随连接存续。
    let mut reader = reader;
    tokio::pin!(shutdown);
    loop {
        let next_frame = tokio::select! {
            result = read_frame(&mut reader) => result,
            _ = &mut writer_done_rx => break,
            _ = &mut shutdown => break,
        };
        let frame = match next_frame {
            Ok(Some(Ok(frame))) => frame,
            Ok(Some(Err(()))) => {
                let resp = JsonRpcResponse::err(
                    Value::Null,
                    JsonRpcError {
                        code: INVALID_REQUEST,
                        message: format!("request too large (max {MAX_FRAME_BYTES} bytes)"),
                        data: None,
                    },
                );
                let _ = tx.send(response_line(resp)).await;
                break;
            }
            Ok(None) | Err(_) => break,
        };
        if frame.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        // ADR-0021：先尝试按反向调用应答路由（result/error + 已知 pending id）；
        // 未命中的帧照常走请求解析。普通客户端从不发送 result/error 帧，
        // 命中失败的开销只是一次廉价预筛。
        if may_be_provider_response(&frame) && route_provider_response(&ctx, &frame, &session) {
            continue;
        }
        let request = match parse_request(&frame) {
            Ok(req) => req,
            Err(resp) => {
                if tx.send(response_line(*resp)).await.is_err() {
                    break;
                }
                continue;
            }
        };
        if request.id.is_null() {
            let _ = dispatch_with_session(&ctx, origin, &mut session, &request).await;
            continue; // JSON-RPC 通知：执行，但不响应。
        }
        let resp = dispatch_with_session(&ctx, origin, &mut session, &request).await;
        if tx.send(response_line(resp)).await.is_err() {
            break;
        }
        if session.close_after_reply() {
            break; // ADR-0017：AUTH_REJECTED 应答后立即关闭，不降级为 User 会话。
        }
    }

    drop(message_listener);
    drop(account_listener);
    drop(intent_listener);
    drop(stopping_listener);
    drop(locale_listener);
    drop(proxy_listener);
    // ADR-0021：连接关闭 → 按属主注销 provider 端点并让其未决调用立即失败。
    if let Some((plugin_id, endpoint)) = session.take_registered() {
        let removed = ctx
            .get::<crate::provider::ProviderRegistry>()
            .map(|registry| registry.remove_if_owner(&plugin_id, &endpoint))
            .unwrap_or(false);
        endpoint.calls().fail_all("provider 连接已关闭");
        if removed {
            eprintln!("[maild/provider] {plugin_id} 的 provider 端点随连接注销");
        }
    }
    drop(tx);
    writer_task.join().await;
}

/// 廉价预筛：可能成为反向应答的帧必含 result 或 error 键名。
fn may_be_provider_response(frame: &[u8]) -> bool {
    const NEEDLES: [&[u8]; 2] = [b"\"result\"", b"\"error\""];
    NEEDLES
        .iter()
        .any(|needle| window_contains(frame, needle))
}

fn window_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// 把入站帧按反向调用应答路由到本会话的 pending 表。返回是否命中。
/// 迟到应答（pending 已被超时移除）静默丢弃并计数；带 method 的帧不是应答。
fn route_provider_response(ctx: &Context, frame: &[u8], session: &SessionAuth) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(frame) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.contains_key("method") || !object.contains_key("id") {
        return false;
    }
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");
    if has_result == has_error {
        return false;
    }
    let Some(id) = object.get("id").filter(|id| !id.is_null()) else {
        return false;
    };
    let reply = if has_result {
        crate::provider::ProviderReply::Ok(object.get("result").cloned().unwrap_or(Value::Null))
    } else if let Some(error) = object.get("error").and_then(|error| error.as_object()) {
        crate::provider::ProviderReply::Err {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(crate::provider::PROVIDER_ERROR),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }
    } else {
        return false;
    };
    if session.provider_calls().route(&id.to_string(), reply) {
        true
    } else if let Some(registry) = ctx.get::<crate::provider::ProviderRegistry>() {
        registry.note_late_response();
        eprintln!(
            "[maild/provider] 迟到应答丢弃 id={id}（累计 {}）",
            registry.late_responses()
        );
        false
    } else {
        false
    }
}

pub async fn serve<R, W>(ctx: Context, reader: R, writer: W)
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    serve_with_origin(ctx, RequestOrigin::Stdio, reader, writer).await;
}

/// 显式指定请求来源的流式服务入口（Unix socket / named pipe 会话用）。
pub async fn serve_with_origin<R, W>(ctx: Context, origin: RequestOrigin, reader: R, writer: W)
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    serve_with_shutdown(ctx, origin, reader, writer, std::future::pending()).await;
}

/// stdio 传输入口：stdin / stdout 行分隔 JSON-RPC。
async fn serve_stdio(ctx: Context) {
    let stdin = BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    serve(ctx, stdin, stdout).await;
}

// —— 本地 IPC 传输 ——

struct StreamSessions {
    tasks: tokio::task::JoinSet<()>,
    shutdown: watch::Sender<bool>,
}

impl StreamSessions {
    fn new() -> Self {
        Self {
            tasks: tokio::task::JoinSet::new(),
            shutdown: watch::channel(false).0,
        }
    }
}

fn spawn_stream_session<S>(
    sessions: &mut StreamSessions,
    stream: S,
    ctx: &Context,
    origin: RequestOrigin,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if sessions.tasks.len() >= MAX_STREAM_SESSIONS {
        return;
    }
    let (read, write) = tokio::io::split(stream);
    let ctx = ctx.clone();
    let mut shutdown_rx = sessions.shutdown.subscribe();
    sessions.tasks.spawn(async move {
        serve_with_shutdown(
            ctx,
            origin,
            BufReader::new(read),
            write,
            async move {
                let _ = shutdown_rx.changed().await;
            },
        )
        .await;
    });
}

async fn close_stream_sessions(sessions: &mut StreamSessions) {
    sessions.shutdown.send_replace(true);
    let joined = tokio::time::timeout(Duration::from_secs(2), async {
        while sessions.tasks.join_next().await.is_some() {}
    })
    .await;
    if joined.is_err() {
        sessions.tasks.abort_all();
        while sessions.tasks.join_next().await.is_some() {}
    }
}

#[cfg(unix)]
struct UnixSocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
    remove_on_drop: bool,
}

#[cfg(unix)]
impl UnixSocketGuard {
    fn capture(path: &Path, remove_on_drop: bool) -> Option<Self> {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

        let metadata = std::fs::symlink_metadata(path).ok()?;
        if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
            return None;
        }
        Some(Self {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            remove_on_drop,
        })
    }

    fn from_listener(listener: &tokio::net::UnixListener) -> Option<Self> {
        let address = listener.local_addr().ok()?;
        Self::capture(address.as_pathname()?, true)
    }
}

#[cfg(unix)]
impl Drop for UnixSocketGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

        if !self.remove_on_drop {
            return;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
fn bind_unix_socket(path: &Path) -> Result<(tokio::net::UnixListener, UnixSocketGuard), String> {
    use std::os::unix::fs::PermissionsExt as _;

    validate_unix_socket_path(&path.to_string_lossy())?;
    let listener = match std::os::unix::net::UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            // A previous process can leave a socket inode behind after a crash,
            // but there is no atomic portable pathname operation that proves an
            // inode is still the one probed before unlink. Refuse every occupied
            // pathname instead of risking deletion of an active listener.
            return Err(format!(
                "api Unix socket 路径已被占用（不会自动删除 stale 文件）: {} ({error})",
                path.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "api Unix socket 绑定 {} 失败: {error}",
                path.display()
            ));
        }
    };
    let guard = UnixSocketGuard::capture(path, true).ok_or_else(|| {
        format!(
            "api Unix socket 绑定后无法确认路径所有权: {}",
            path.display()
        )
    })?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("api Unix socket 设置权限 {} 失败: {error}", path.display()))?;
    listener.set_nonblocking(true).map_err(|error| {
        format!(
            "api Unix socket 设置 nonblocking {} 失败: {error}",
            path.display()
        )
    })?;
    let listener = tokio::net::UnixListener::from_std(listener).map_err(|error| {
        format!(
            "api Unix socket listener 初始化 {} 失败: {error}",
            path.display()
        )
    })?;
    Ok((listener, guard))
}

#[cfg(unix)]
async fn serve_unix_guarded<F>(
    listener: tokio::net::UnixListener,
    ctx: Context,
    shutdown: F,
    _socket_guard: Option<UnixSocketGuard>,
) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let mut sessions = StreamSessions::new();
    tokio::pin!(shutdown);
    let result = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    spawn_stream_session(&mut sessions, stream, &ctx, RequestOrigin::UnixSocket)
                }
                Err(error) => break Err(error),
            },
            joined = sessions.tasks.join_next(), if !sessions.tasks.is_empty() => {
                if let Some(Err(error)) = joined {
                    eprintln!("[maild/api] IPC session task failed: {error}");
                }
            }
        }
    };
    close_stream_sessions(&mut sessions).await;
    result
}

/// 在已绑定的 Unix domain socket 上提供多客户端 JSON-RPC。
///
/// 关闭时会结束已接受的会话，并仅在路径仍指向该 listener 时删除
/// socket 文件，避免删掉之后创建的新 listener。
#[cfg(unix)]
pub async fn serve_unix<F>(
    listener: tokio::net::UnixListener,
    ctx: Context,
    shutdown: F,
) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let guard = UnixSocketGuard::from_listener(&listener);
    serve_unix_guarded(listener, ctx, shutdown, guard).await
}

/// `serve_unix` 的显式别名。
#[cfg(unix)]
pub async fn serve_unix_socket<F>(
    listener: tokio::net::UnixListener,
    ctx: Context,
    shutdown: F,
) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    serve_unix(listener, ctx, shutdown).await
}

#[cfg(windows)]
fn create_named_pipe(
    name: &str,
    first: bool,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    let mut security = NamedPipeSecurity::for_current_user()?;
    // Tokio's `create` passes a null SECURITY_ATTRIBUTES pointer, which gives
    // the pipe the default (often NULL-DACL) security descriptor. Keep the
    // descriptor alive through CreateNamedPipeW; the OS copies it on create.
    // Residual: this scopes access to the current Windows user SID (and keeps
    // remote clients rejected), so separate logon sessions of that same user
    // are intentionally not distinguished by this portable Tokio API.
    unsafe {
        options.create_with_security_attributes_raw(
            name,
            &mut security.attributes as *mut _ as *mut std::ffi::c_void,
        )
    }
}

#[cfg(windows)]
struct NamedPipeSecurity {
    attributes: windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
}

#[cfg(windows)]
impl NamedPipeSecurity {
    fn for_current_user() -> io::Result<Self> {
        use std::mem::size_of;
        use std::ptr::null_mut;
        use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{
            GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        let mut token = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let result = (|| {
            let mut required = 0;
            // The first query intentionally supplies no buffer to obtain the
            // variable-sized TOKEN_USER record.
            unsafe {
                GetTokenInformation(token, TokenUser, null_mut(), 0, &mut required);
            }
            if required == 0 {
                return Err(io::Error::last_os_error());
            }
            let word_count = (required as usize).div_ceil(size_of::<u64>());
            let mut token_user = vec![0u64; word_count];
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    token_user.as_mut_ptr() as *mut _,
                    (token_user.len() * size_of::<u64>()) as u32,
                    &mut required,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let token_user = unsafe { &*(token_user.as_ptr() as *const TOKEN_USER) };
            let mut sid_string = null_mut();
            if unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid_string) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let sid = unsafe {
                let mut length = 0;
                while *sid_string.add(length) != 0 {
                    length += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(sid_string, length))
            };
            unsafe { LocalFree(sid_string as _) };

            let sddl = format!("O:{sid}G:{sid}D:P(A;;GA;;;{sid})");
            let mut sddl_w: Vec<u16> = sddl.encode_utf16().collect();
            sddl_w.push(0);
            let mut descriptor = null_mut();
            let mut descriptor_size = 0;
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl_w.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    &mut descriptor_size,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if descriptor_size == 0 || descriptor.is_null() {
                unsafe { LocalFree(descriptor as _) };
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Windows named pipe security descriptor is empty",
                ));
            }
            Ok(Self {
                attributes: windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
                    nLength: size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: descriptor,
                    bInheritHandle: 0,
                },
                descriptor,
            })
        })();
        unsafe { CloseHandle(token) };
        result
    }
}

#[cfg(windows)]
impl Drop for NamedPipeSecurity {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::LocalFree;

        if !self.descriptor.is_null() {
            unsafe { LocalFree(self.descriptor as _) };
        }
    }
}

#[cfg(windows)]
async fn serve_named_pipe_from<F>(
    name: String,
    mut server: tokio::net::windows::named_pipe::NamedPipeServer,
    ctx: Context,
    shutdown: F,
) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let mut sessions = StreamSessions::new();
    tokio::pin!(shutdown);
    let result = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            connected = server.connect() => {
                connected?;
                let stream = server;
                server = create_named_pipe(&name, false)?;
                spawn_stream_session(&mut sessions, stream, &ctx, RequestOrigin::NamedPipe);
            },
            joined = sessions.tasks.join_next(), if !sessions.tasks.is_empty() => {
                if let Some(Err(error)) = joined {
                    eprintln!("[maild/api] IPC session task failed: {error}");
                }
            }
        }
    };
    close_stream_sessions(&mut sessions).await;
    result
}

/// 在 Windows named pipe 上提供多客户端 JSON-RPC。
#[cfg(windows)]
pub async fn serve_named_pipe<F>(name: String, ctx: Context, shutdown: F) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    validate_named_pipe_name(&name)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let server = create_named_pipe(&name, true)?;
    serve_named_pipe_from(name, server, ctx, shutdown).await
}

// —— HTTP 传输 ——

#[derive(Clone)]
struct HttpState {
    ctx: Context,
    token: Arc<str>,
}

fn bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    let Some(presented) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    presented.len() == expected.len() && bool::from(presented.as_bytes().ct_eq(expected.as_bytes()))
}

async fn require_bearer(
    State(state): State<HttpState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    if bearer_matches(&headers, &state.token) {
        return next.run(request).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
    )
        .into_response()
}

async fn http_rpc(State(state): State<HttpState>, body: Bytes) -> Response {
    let request = match parse_request(&body) {
        Ok(request) => request,
        Err(response) => {
            return (StatusCode::OK, Json(bounded_response(*response))).into_response();
        }
    };
    if request.id.is_null() {
        let _ = dispatch_with_origin(&state.ctx, RequestOrigin::Http, &request).await;
        return StatusCode::NO_CONTENT.into_response();
    }
    let response = dispatch_with_origin(&state.ctx, RequestOrigin::Http, &request).await;
    (StatusCode::OK, Json(bounded_response(response))).into_response()
}

fn http_router(ctx: Context, token: String) -> Router {
    let state = HttpState {
        ctx,
        token: Arc::from(token),
    };
    Router::new()
        .route("/rpc", post(http_rpc))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ))
        .layer(DefaultBodyLimit::max(MAX_FRAME_BYTES))
        .with_state(state)
}

/// 在已绑定的回环 listener 上提供 JSON-RPC HTTP；主要供插件和 hermetic 测试复用。
pub async fn serve_http<F>(
    listener: tokio::net::TcpListener,
    ctx: Context,
    token: String,
    shutdown: F,
) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let local_addr = listener.local_addr()?;
    if !local_addr.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("api HTTP 只允许回环地址，收到: {local_addr}"),
        ));
    }
    validate_bearer_token(&token)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    axum::serve(listener, http_router(ctx, token))
        .with_graceful_shutdown(shutdown)
        .await
}

// —— 插件 ——

/// api 插件：inject 配置 → 启动 stdio、HTTP 或本地 IPC JSON-RPC server。
pub struct ApiPlugin;

impl Plugin for ApiPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<ApiConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        // 尽早固定进程启动时刻，供 getAgentInfo 诊断字段使用（ADR-0016）。
        started_at_epoch_millis();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "api 插件需要在 tokio 运行时内挂载（用守护进程运行）")?;
        let cfg = ctx.get::<ApiConfig>().expect("inject 就绪");
        validate_api_config(&cfg)?;
        let (task, shutdown) = match cfg.transport.as_str() {
            "stdio" => {
                let ctx2 = ctx.clone();
                let task = handle.spawn(async move {
                    serve_stdio(ctx2).await;
                });
                (task, None)
            }
            "http" => {
                let (listen, token) = validate_http_config(&cfg)?;
                let listener = std::net::TcpListener::bind(listen)
                    .map_err(|e| format!("api HTTP 绑定 {listen} 失败: {e}"))?;
                let bound_addr = listener
                    .local_addr()
                    .map_err(|e| format!("api HTTP 读取监听地址失败: {e}"))?;
                listener
                    .set_nonblocking(true)
                    .map_err(|e| format!("api HTTP 设置 nonblocking 失败: {e}"))?;
                let listener = tokio::net::TcpListener::from_std(listener)
                    .map_err(|e| format!("api HTTP listener 初始化失败: {e}"))?;
                let (shutdown_tx, shutdown_rx) = oneshot::channel();
                let ctx2 = ctx.clone();
                eprintln!("[maild/api] HTTP listening on http://{bound_addr}/rpc");
                let task = handle.spawn(async move {
                    if let Err(e) = serve_http(listener, ctx2, token, async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    {
                        eprintln!("[maild] api HTTP 服务退出: {e}");
                    }
                });
                (task, Some(shutdown_tx))
            }
            #[cfg(unix)]
            "unix" | "unix_socket" | "socket" | "ipc" => {
                let path = PathBuf::from(&cfg.listen);
                let (listener, guard) = bind_unix_socket(&path)?;
                let (shutdown_tx, shutdown_rx) = oneshot::channel();
                let ctx2 = ctx.clone();
                eprintln!("[maild/api] Unix socket listening on {}", path.display());
                let task = handle.spawn(async move {
                    if let Err(error) = serve_unix_guarded(
                        listener,
                        ctx2,
                        async move {
                            let _ = shutdown_rx.await;
                        },
                        Some(guard),
                    )
                    .await
                    {
                        eprintln!("[maild] api Unix socket 服务退出: {error}");
                    }
                });
                (task, Some(shutdown_tx))
            }
            #[cfg(windows)]
            "named_pipe" | "pipe" | "ipc" => {
                let name = cfg.listen.clone();
                let server = create_named_pipe(&name, true)
                    .map_err(|error| format!("api named pipe 创建 {name} 失败: {error}"))?;
                let (shutdown_tx, shutdown_rx) = oneshot::channel();
                let ctx2 = ctx.clone();
                eprintln!("[maild/api] named pipe listening on {name}");
                let task = handle.spawn(async move {
                    if let Err(error) = serve_named_pipe_from(name, server, ctx2, async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    {
                        eprintln!("[maild] api named pipe 服务退出: {error}");
                    }
                });
                (task, Some(shutdown_tx))
            }
            _ => unreachable!("transport was validated before dispatch"),
        };

        ctx.effect_async(async move {
            let mut task = task;
            if let Some(shutdown) = shutdown {
                let _ = shutdown.send(());
                if tokio::time::timeout(Duration::from_secs(5), &mut task)
                    .await
                    .is_err()
                {
                    task.abort();
                    let _ = task.await;
                }
            } else {
                task.abort();
                let _ = task.await;
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod agent_control_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::Poll;

    use cordis::FiberState;
    use serde_json::json;

    /// ADR-0048 决策 3：写队列摘要的固定说明带稳定 code，服务器原文不进摘要。
    #[test]
    fn pending_op_summaries_carry_a_stable_detail_code() {
        use crate::pending::{OpKind, OpState, PendingOp};
        let op = |state, detail: &str| PendingOp {
            id: 1,
            account: "demo".into(),
            folder_id: 1,
            folder: "INBOX".into(),
            uid: "7".into(),
            kind: OpKind::Move,
            payload: r#"{"target":"Archive"}"#.into(),
            state,
            attempts: 3,
            created_at: 1,
            updated_at: 1,
            detail: detail.into(),
        };
        for (state, detail, code) in [
            (OpState::Failed, "用户放弃", Some("discarded")),
            (OpState::Failed, "NO [TRYCREATE] 文件夹不存在", Some("rejected")),
            (OpState::Unknown, "连接中断", Some("unconfirmed")),
            (OpState::Pending, "", None),
        ] {
            let summary = pending_op_summary_json(&op(state, detail), None);
            assert_eq!(summary.get("detail_code").and_then(Value::as_str), code, "{summary}");
            assert!(!summary["detail"].as_str().unwrap().contains("TRYCREATE"));
            assert!(summary.get("server_detail").is_none(), "{summary}");
        }
        assert!(pending_op_json(&op(OpState::Failed, "x")).get("detail_code").is_none());

        // ADR-0048 L6：收信循环写下的服务器命令失败给 `server_rejected` 与去掉前缀的原文；
        // `detail` 仍是老前端看到的固定句子。
        for (state, detail, code, server) in [
            (OpState::Failed, "APPEND 失败: No Response: [TRYCREATE] gone", "server_rejected", "No Response: [TRYCREATE] gone"),
            (OpState::Failed, "STORE 失败: Bad Response: bad flag", "server_rejected", "Bad Response: bad flag"),
            (OpState::Failed, "MOVE 失败: No Response: quota", "server_rejected", "No Response: quota"),
            (OpState::Failed, "COPY 失败: No Response: quota", "server_rejected", "No Response: quota"),
            (OpState::Failed, "SELECT INBOX 失败: No Response: missing", "server_rejected", "No Response: missing"),
            (OpState::Unknown, "COPY 已成功但标记删除失败: connection lost", "unconfirmed", "connection lost"),
        ] {
            let summary = pending_op_summary_json(&op(state, detail), None);
            assert_eq!(summary["detail_code"], code, "{summary}");
            assert_eq!(summary["server_detail"], server, "{summary}");
            assert!(!summary["detail"].as_str().unwrap().contains(server), "{summary}");
        }
        // 文件夹名不同的 SELECT 前缀不是这个 op 的服务器失败。
        let other = pending_op_summary_json(&op(OpState::Failed, "SELECT Other 失败: x"), None);
        assert_eq!(other["detail_code"], "rejected");
        assert_eq!(
            pending_op_summary_json(&op(OpState::Failed, "APPEND 失败: No"), None)["detail"],
            "服务器明确拒绝或重试次数已达上限，请核对账户与目标文件夹。"
        );
    }

    #[test]
    fn method_policy_table_pins_every_dispatched_method() {
        use MethodPolicy::{Capability, Open, UserOnly};
        let cases: &[(&str, MethodPolicy)] = &[
            ("authenticate", Open),
            ("whoami", Open),
            ("getAgentInfo", Open),
            // ADR-0048 决策 2：读偏好任何主体可用；写偏好与 setSettings 同域，不新增授权词。
            ("getLocalePreference", Open),
            ("setLocalePreference", Capability("settings_write")),
            ("listAccounts", Capability("messages_read")),
            ("listFolders", Capability("messages_read")),
            // ADR-0030 决策 4：信箱写回有自己的词，既不是 messages_read 也不是 messages_send。
            ("markMessages", Capability("mailbox_write")),
            ("moveMessages", Capability("mailbox_write")),
            ("deleteMessages", Capability("mailbox_write")),
            ("listPendingOps", Capability("mailbox_write")),
            ("resolvePendingOp", Capability("mailbox_write")),
            // 纯读：拼草稿不落库、不投递（决策 8）。
            ("prepareReply", Capability("messages_read")),
            // ADR-0032 决策 4：附件字节属于同一封信的同一份内容；策略是应用级开关。
            ("readAttachment", Capability("messages_read")),
            ("getReadingPolicy", Capability("settings_read")),
            ("setReadingPolicy", Capability("settings_write")),
            ("getProxyConfig", Open),
            ("setProxyConfig", Capability("settings_write")),
            ("testProxy", Capability("settings_write")),
            // ADR-0034 决策 7：草稿自成一域，读写两个词。并进 messages_read 会让只申请
            // 「读邮件」的主体看到用户正在写什么；并进 messages_send 会让能发信的主体翻草稿箱。
            ("listDrafts", Capability("drafts_read")),
            ("getDraft", Capability("drafts_read")),
            ("createDraft", Capability("drafts_write")),
            ("updateDraft", Capability("drafts_write")),
            ("deleteDraft", Capability("drafts_write")),
            ("sendDraft", Capability("messages_send")),
            ("resolveDraftSend", Capability("messages_send")),
            ("beginAttachmentUpload", Capability("drafts_write")),
            ("writeAttachmentUpload", Capability("drafts_write")),
            ("finishAttachmentUpload", Capability("drafts_write")),
            ("cancelAttachmentUpload", Capability("drafts_write")),
            ("listMessageSummaries", Capability("messages_read")),
            ("listMessages", Capability("messages_read")),
            ("getMessage", Capability("messages_read")),
            // ADR-0029 决策 12：换检索维度不构成新的权限面。
            ("searchMessages", Capability("messages_read")),
            ("getThread", Capability("messages_read")),
            ("getSearchIndexStatus", Capability("messages_read")),
            ("send", Capability("messages_send")),
            ("getSettings", Capability("settings_read")),
            ("setSettings", Capability("settings_write")),
            ("getNotificationStatus", Capability("notifications_read")),
            ("listFrontends", Capability("frontend_open")),
            ("claimNextOpenIntent", Capability("frontend_open")),
            ("openFrontend", Capability("frontend_open")),
            // 不比 getAgentInfo + listAccounts 多暴露什么（决策 8）。
            ("status", Capability("messages_read")),
            // 受管账户是 M3.7 之后发信的正路，选自己的账户不比「能发信」更大（决策 8）。
            ("sendFromAccount", Capability("messages_send")),
            // 只能触发系统提示，不能授予自己权限（决策 8）。
            ("requestNotificationAuthorization", Capability("settings_write")),
            // ADR-0030 决策 4/5：三个受托管理面，仅前端 manifest 可声明。
            ("listAccountProfiles", Capability("accounts_manage")),
            ("probeAccount", Capability("accounts_manage")),
            ("createAccount", Capability("accounts_manage")),
            ("updateAccount", Capability("accounts_manage")),
            ("setAccountEnabled", Capability("accounts_manage")),
            ("removeAccount", Capability("accounts_manage")),
            ("listAddresses", Capability("addresses_manage")),
            ("createAddress", Capability("addresses_manage")),
            ("updateAddress", Capability("addresses_manage")),
            ("removeAddress", Capability("addresses_manage")),
            ("listExternalPlugins", Capability("plugins_manage")),
            ("openExternalPluginUi", Capability("plugins_manage")),
            ("commitPluginInstall", Capability("plugins_manage")),
            ("removePlugin", Capability("plugins_manage")),
            ("rollbackPlugin", Capability("plugins_manage")),
            ("setPluginEnabled", Capability("plugins_manage")),
            ("refreshPlugins", Capability("plugins_manage")),
            // ADR-0021：仅 external manifest 可声明；frontend 侧既无路由也无授权词。
            ("providerRegister", Capability("transport_provider")),
            // 提权防线（决策 6），另有 ADR-0039 原生维护例外。持证者不得铸证，否则声明上限
            // 可以自己给自己改。
            ("revealPluginToken", UserOnly),
            ("repairBuiltinFrontend", UserOnly),
            ("resyncBuiltinFrontend", UserOnly),
            ("stopManagedAgent", UserOnly),
        ];
        for (method, expected) in cases {
            assert_eq!(&method_policy(method), expected, "policy of {method}");
        }
        // fail-closed 默认：未登记的未来方法对插件主体一律拒绝。
        assert_eq!(method_policy("someFutureMethod"), UserOnly);
    }

    #[test]
    fn denial_uses_dedicated_error_code_and_keeps_connection() {
        // -32016 与 -32014 语义分离：授权拒绝不断开连接。
        assert_ne!(METHOD_NOT_PERMITTED, AUTH_REJECTED);
        assert_eq!(METHOD_NOT_PERMITTED, -32016);
    }

    fn mount_account_manager(ctx: &Context) -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let catalog_path = dir.path().join("accounts.toml");
        let accounts = Arc::new(crate::accounts::Accounts::with_entries(Vec::new()));
        let manager = crate::accounts::AccountManager::start(
            ctx,
            crate::accounts::ManagedAccountsOptions {
                catalog_path: catalog_path.clone(),
            },
            crate::account_catalog::AccountCatalog::empty(),
            accounts,
            Vec::new(),
        )
        .unwrap();
        // tempdir 在函数末尾释放；把目录留在原处由调用方管理。
        std::mem::forget(dir);
        ctx.provide::<crate::accounts::AccountManager>(manager);
        catalog_path
    }

    #[tokio::test(flavor = "current_thread")]
    async fn account_profile_methods_require_manager_and_validate_params() {
        let ctx = Context::new();
        let missing =
            dispatch(&ctx, &request("listAccountProfiles", json!({}), json!(1))).await;
        assert_eq!(missing.error.unwrap().code, SERVICE_UNAVAILABLE);

        mount_account_manager(&ctx);

        let empty = dispatch(&ctx, &request("listAccountProfiles", json!({}), json!(2)))
            .await
            .result
            .unwrap();
        assert_eq!(empty["profiles"], json!([]));

        let bad_params = dispatch(
            &ctx,
            &request("listAccountProfiles", json!({ "unknown": true }), json!(3)),
        )
        .await;
        assert_eq!(bad_params.error.unwrap().code, INVALID_PARAMS);

        let archived = dispatch(
            &ctx,
            &request("listAccountProfiles", json!({ "include_archived": true }), json!(4)),
        )
        .await
        .result
        .unwrap();
        assert_eq!(archived["profiles"], json!([]));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn secret_account_mutations_are_gated_by_trusted_origin() {
        let ctx = Context::new();
        mount_account_manager(&ctx);
        let params = json!({
            "draft": {
                "display_name": "Main",
                "address": "user@example.test",
                "incoming": {
                    "kind": "imap",
                    "server": "127.0.0.1",
                    "port": 1,
                    "security": "implicit",
                    "username": "user@example.test",
                    "mailbox": "INBOX",
                    "idle": true,
                    "poll_secs": 60
                }
            },
            "credentials": { "incoming": "secret-password" }
        });

        for (origin, code) in [
            (RequestOrigin::Stdio, INVALID_REQUEST),
            (RequestOrigin::Http, INVALID_REQUEST),
            (RequestOrigin::NamedPipe, INVALID_REQUEST),
        ] {
            let response = dispatch_with_origin(
                &ctx,
                origin,
                &request("createAccount", params.clone(), json!(1)),
            )
            .await;
            let error = response.error.unwrap();
            assert_eq!(error.code, code, "{origin:?}");
            assert!(
                !error.message.contains("secret-password"),
                "错误信息不得回显秘密"
            );
        }

        // Unix socket 来源放行进入业务层（此处因探测失败返回 ACCOUNT_PROBE_FAILED，
        // 证明请求已穿过传输门禁）。
        let allowed = dispatch_with_origin(
            &ctx,
            RequestOrigin::UnixSocket,
            &request("createAccount", params, json!(2)),
        )
        .await;
        assert_eq!(allowed.error.unwrap().code, ACCOUNT_PROBE_FAILED);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn account_error_codes_are_stable() {
        let ctx = Context::new();
        mount_account_manager(&ctx);

        let enabled = dispatch(
            &ctx,
            &request(
                "setAccountEnabled",
                json!({ "id": "acct_aaaaaaaaaaaaaaaaaaaaaa", "expected_revision": 1, "enabled": false }),
                json!(1),
            ),
        )
        .await;
        assert_eq!(enabled.error.unwrap().code, ACCOUNT_NOT_FOUND);

        let remove = dispatch(
            &ctx,
            &request(
                "removeAccount",
                json!({ "id": "acct_aaaaaaaaaaaaaaaaaaaaaa", "expected_revision": 0 }),
                json!(2),
            ),
        )
        .await;
        assert_eq!(remove.error.unwrap().code, ACCOUNT_NOT_FOUND);

        let send = dispatch(
            &ctx,
            &request(
                "sendFromAccount",
                json!({ "account": "missing", "to": "b@test.local" }),
                json!(3),
            ),
        )
        .await;
        assert_eq!(send.error.unwrap().code, ACCOUNT_NOT_FOUND);
    }



    use crate::events::{MailMessage, NotifyRoute, NotifyRouteEvent};
    use crate::frontend::{
        FrontendBroker, FrontendConfig, OpenIntent, BUILTIN_RECOVERY_FRONTEND,
    };
    use crate::rules::{RulesConfig, RulesPlugin};
    use crate::store::{Store, StoreConfig};
    use crate::transport::{TransportConfig, TransportPlugin};

    fn request(method: &str, params: Value, id: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
            id,
        }
    }

    struct FailingWriter;

    impl AsyncWrite for FailingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed")))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn status_without_accounts_reports_empty() {
        let ctx = Context::new();
        ctx.provide::<Store>(Arc::new(Store::open("").unwrap()));
        let resp = dispatch(&ctx, &request("status", json!({}), json!(1))).await;
        assert_eq!(resp.id, json!(1));
        let r = resp.result.expect("成功");
        assert_eq!(r["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(r["message_count"], 0);
        assert_eq!(r["accounts"].as_array().unwrap().len(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_info_reports_identity_and_protocol_generation() {
        let ctx = Context::new();
        let resp = dispatch(&ctx, &request("getAgentInfo", json!({}), json!(21))).await;
        let r = resp.result.expect("无服务依赖，任何装配下都应成功");
        assert_eq!(r["protocol_version"], maild_client::PROTOCOL_VERSION);
        assert_eq!(r["version"], env!("CARGO_PKG_VERSION"));
        assert!(r["pid"].as_u64().unwrap() > 0);
        assert!(r["started_at"].as_u64().unwrap() > 0);
        assert!(!r["executable_path"].as_str().unwrap().is_empty());
        // 开发构建未注入 MAILD_BUILD_REV 时省略该字段（ADR-0016）。
        assert!(r.get("build_rev").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_info_rejects_params_but_serves_every_origin() {
        let ctx = Context::new();
        let bad = dispatch(&ctx, &request("getAgentInfo", json!({"x": 1}), json!(22))).await;
        assert_eq!(bad.error.unwrap().code, INVALID_PARAMS);
        for origin in [RequestOrigin::Stdio, RequestOrigin::Http] {
            let resp =
                dispatch_with_origin(&ctx, origin, &request("getAgentInfo", json!({}), json!(23)))
                    .await;
            assert!(
                resp.error.is_none(),
                "{origin:?} 应可达: {:?}",
                resp.error
            );
            assert_eq!(
                resp.result.unwrap()["protocol_version"],
                maild_client::PROTOCOL_VERSION
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn notification_authorization_methods_are_explicit_and_strict() {
        let ctx = Context::new();
        let invalid = dispatch(
            &ctx,
            &request("getNotificationStatus", json!({ "extra": true }), json!(1)),
        )
        .await;
        assert_eq!(invalid.error.unwrap().code, INVALID_PARAMS);

        let unavailable =
            dispatch(&ctx, &request("getNotificationStatus", json!({}), json!(2))).await;
        assert_eq!(unavailable.error.unwrap().code, SERVICE_UNAVAILABLE);

        ctx.provide::<Notifier>(Arc::new(Notifier::new("dev-log").unwrap()));
        let status = dispatch(&ctx, &request("getNotificationStatus", json!({}), json!(3)))
            .await
            .result
            .unwrap();
        assert_eq!(status["provider"], "dev-log");
        assert_eq!(status["availability"], "available");
        assert_eq!(status["authorization"], "not_applicable");

        let requested = dispatch(
            &ctx,
            &request("requestNotificationAuthorization", json!({}), json!(4)),
        )
        .await
        .result
        .unwrap();
        assert_eq!(requested["authorization"], "not_applicable");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn frontend_intents_are_strict_bounded_and_one_shot() {
        let ctx = Context::new();
        let external_unavailable =
            dispatch(&ctx, &request("listExternalPlugins", json!({}), json!(1))).await;
        assert_eq!(
            external_unavailable.error.unwrap().code,
            SERVICE_UNAVAILABLE
        );
        ctx.provide::<crate::external::ExternalPluginManager>(Arc::new(
            crate::external::ExternalPluginManager::empty(),
        ));
        let external = dispatch(&ctx, &request("listExternalPlugins", json!({}), json!(1)))
            .await
            .result
            .unwrap();
        assert_eq!(external["plugins"], json!([]));
        let unavailable = dispatch(
            &ctx,
            &request(
                "claimNextOpenIntent",
                json!({ "frontend": "test.reader" }),
                json!(1),
            ),
        )
        .await;
        assert_eq!(unavailable.error.unwrap().code, SERVICE_UNAVAILABLE);

        // An installed independent reader: the bare daemon queues without platform activation.
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("test.reader");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(
            package.join("frontend.toml"),
            "schema_version = 1\nprotocol_version = 1\nid = \"test.reader\"\nname = \"Reader\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"inbox\", \"message\"]\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for (path, mode) in [(root.path(), 0o700), (package.as_path(), 0o700)] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
            }
            std::fs::set_permissions(
                package.join("frontend.toml"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let broker = Arc::new(
            FrontendBroker::with_install_root(
                &FrontendConfig {
                    default: "test.reader".into(),
                },
                Some(root.path()),
            )
            .unwrap(),
        );
        let intent = OpenIntent::Message {
            folder: None,
            account: "main".into(),
            uid: "42".into(),
        };
        ctx.provide::<FrontendBroker>(broker);

        let catalog = dispatch(&ctx, &request("listFrontends", json!({}), json!(2)))
            .await
            .result
            .unwrap();
        assert_eq!(catalog["default"], "test.reader");
        assert_eq!(catalog["frontends"][0]["id"], BUILTIN_RECOVERY_FRONTEND);
        assert_eq!(catalog["frontends"][1]["id"], "test.reader");

        let missing = dispatch(
            &ctx,
            &request(
                "openFrontend",
                json!({
                    "frontend": "org.example.missing",
                    "intent": { "route": "inbox" }
                }),
                json!(2),
            ),
        )
        .await;
        assert_eq!(missing.error.unwrap().code, INVALID_PARAMS);

        let rejected = dispatch(
            &ctx,
            &request(
                "openFrontend",
                json!({ "intent": { "route": "future_route" } }),
                json!(2),
            ),
        )
        .await;
        assert_eq!(rejected.error.unwrap().code, INVALID_PARAMS);

        // Explicit requests keep their one-shot intent; an independent frontend is not activated.
        // Default activation failures are separately covered as native recovery.
        let opened = dispatch(
            &ctx,
            &request(
                "openFrontend",
                json!({ "frontend": "test.reader", "intent": intent.clone() }),
                json!(3),
            ),
        )
        .await
        .result
        .unwrap();
        assert_eq!(opened["queued"], true);
        assert_eq!(opened["activation_requested"], false);

        let invalid = dispatch(
            &ctx,
            &request(
                "claimNextOpenIntent",
                json!({ "frontend": "../evil" }),
                json!(4),
            ),
        )
        .await;
        assert_eq!(invalid.error.unwrap().code, INVALID_PARAMS);

        let claimed = dispatch(
            &ctx,
            &request(
                "claimNextOpenIntent",
                json!({ "frontend": "test.reader" }),
                json!(5),
            ),
        )
        .await
        .result
        .unwrap();
        assert_eq!(claimed["intent"], serde_json::to_value(intent).unwrap());

        let empty = dispatch(
            &ctx,
            &request(
                "claimNextOpenIntent",
                json!({ "frontend": "test.reader" }),
                json!(6),
            ),
        )
        .await
        .result
        .unwrap();
        assert!(empty["intent"].is_null());
    }

    fn mount_rules(ctx: &Context) {
        let config = RulesConfig::default();
        ctx.provide::<RulesConfig>(Arc::new(config.clone()));
        ctx.plugin(Arc::new(RulesPlugin::new(&config)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn settings_patch_roundtrips_and_preserves_fields() {
        let ctx = Context::new();
        mount_rules(&ctx);

        let response = dispatch(
            &ctx,
            &request(
                "setSettings",
                json!({
                    "rules": {
                        "show_sensitive": false,
                        "silent_keywords": ["private"]
                    }
                }),
                json!(1),
            ),
        )
        .await;
        let result = response.result.expect("设置成功");
        assert_eq!(result["rules"]["show_sensitive"], false);
        assert_eq!(result["rules"]["timezone_offset_minutes"], 0);
        assert_eq!(result["persisted"], false);

        let response = dispatch(&ctx, &request("getSettings", json!({}), json!(2))).await;
        let result = response.result.expect("读取成功");
        assert_eq!(result["rules"]["show_sensitive"], false);
        assert_eq!(result["rules"]["silent_keywords"], json!(["private"]));
        assert_eq!(result["rules"]["quiet_hours"], json!([]));

        let route = ctx
            .bail::<NotifyRouteEvent>(NotifyRoute {
                channel: "system".into(),
                sensitive: true,
                subject: "runtime settings".into(),
                from: "sender@example.test".into(),
            })
            .expect("runtime rule should decide");
        assert_eq!(route.channel, "silent", "patch immediately affects routing");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn settings_reject_unknown_and_invalid_rules() {
        let ctx = Context::new();
        mount_rules(&ctx);

        let response = dispatch(
            &ctx,
            &request(
                "setSettings",
                json!({ "rules": { "unknown": true } }),
                json!(1),
            ),
        )
        .await;
        assert_eq!(response.error.unwrap().code, INVALID_PARAMS);

        let response = dispatch(
            &ctx,
            &request(
                "setSettings",
                json!({
                    "rules": {
                        "quiet_hours": [{ "start": "25:00", "end": "07:00" }]
                    }
                }),
                json!(2),
            ),
        )
        .await;
        assert_eq!(response.error.unwrap().code, INVALID_PARAMS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn settings_without_rules_are_unavailable() {
        let ctx = Context::new();
        let get = dispatch(&ctx, &request("getSettings", json!({}), json!(1))).await;
        assert_eq!(get.error.unwrap().code, SERVICE_UNAVAILABLE);
        let set = dispatch(
            &ctx,
            &request(
                "setSettings",
                json!({ "rules": { "show_sensitive": false } }),
                json!(2),
            ),
        )
        .await;
        assert_eq!(set.error.unwrap().code, SERVICE_UNAVAILABLE);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_messages_paginates_recent_first() {
        let ctx = Context::new();
        let store = Arc::new(Store::open("").unwrap());
        for i in 1..=3u64 {
            store
                .insert_message(&MailMessage {
                    account: "a".into(),
                    uid: i.to_string(),
                    sender: "s".into(),
                    recipient: String::new(),
                    subject: format!("sub{i}"),
                    body: format!("b{i}"),
                    received_at: i,
                    ..Default::default()
                })
                .unwrap();
        }
        ctx.provide::<Store>(store);
        let resp = dispatch(
            &ctx,
            &request("listMessages", json!({ "limit": 2 }), json!(1)),
        )
        .await;
        let r = resp.result.expect("成功");
        assert_eq!(r["total"], 3);
        let msgs = r["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        // 最近在前：uid 3、2
        assert_eq!(msgs[0]["uid"], "3");
        assert_eq!(msgs[1]["uid"], "2");

        for params in [
            json!({ "limit": "2" }),
            json!({ "limit": 0 }),
            json!({ "limit": 501 }),
            json!({ "offset": -1 }),
            json!({ "offset": 1.5 }),
            json!({ "offset": (i64::MAX as u64) + 1 }),
            json!({ "unexpected": true }),
            json!([]),
        ] {
            let response = dispatch(&ctx, &request("listMessages", params, json!(2))).await;
            assert_eq!(response.error.unwrap().code, INVALID_PARAMS);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_messages_matches_chinese_and_applies_structured_filters() {
        let ctx = Context::new();
        let store = Arc::new(Store::open("").unwrap());
        for (uid, subject, sender, attachments) in [
            ("1", "发票已开具", "zhang@example.test", true),
            ("2", "会议纪要", "li@example.test", false),
        ] {
            store
                .insert_message(&MailMessage {
                    account: "a".into(),
                    uid: uid.into(),
                    sender: sender.into(),
                    subject: subject.into(),
                    body: "正文".into(),
                    received_at: 100,
                    has_attachments: attachments,
                    ..Default::default()
                })
                .unwrap();
        }
        ctx.provide::<Store>(store);

        let result = dispatch(
            &ctx,
            &request("searchMessages", json!({ "query": "发票" }), json!(1)),
        )
        .await
        .result
        .expect("检索成功");
        assert_eq!(result["total"], 1);
        assert_eq!(result["scope"], "local");
        assert_eq!(result["messages"][0]["uid"], "1");

        let filtered = dispatch(
            &ctx,
            &request(
                "searchMessages",
                json!({ "from": "li@", "hasAttachments": false }),
                json!(2),
            ),
        )
        .await
        .result
        .expect("纯过滤也成立");
        assert_eq!(filtered["messages"][0]["uid"], "2");

        for params in [
            json!({ "limit": 0 }),
            json!({ "limit": 201 }),
            json!({ "offset": -1 }),
            json!({ "scope": "remote" }),
            json!({ "since": -1 }),
            json!({ "hasAttachments": "yes" }),
            json!({ "unexpected": true }),
            json!([]),
            // 服务器检索必须落到一个具体信箱，且要有查询词。
            json!({ "scope": "server" }),
            json!({ "scope": "server", "account": "a" }),
            json!({ "scope": "server", "account": "a", "folder": "INBOX" }),
        ] {
            let response = dispatch(&ctx, &request("searchMessages", params, json!(3))).await;
            assert_eq!(response.error.unwrap().code, INVALID_PARAMS);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_thread_returns_the_whole_conversation_and_index_status_is_readable() {
        let ctx = Context::new();
        let store = Arc::new(Store::open("").unwrap());
        for (uid, message_id, in_reply_to) in [("1", "a@x", ""), ("2", "b@x", "a@x")] {
            store
                .insert_message(&MailMessage {
                    account: "a".into(),
                    uid: uid.into(),
                    sender: "s".into(),
                    subject: "会议安排".into(),
                    body: "正文".into(),
                    received_at: 100,
                    message_id: message_id.into(),
                    in_reply_to: in_reply_to.into(),
                    references: in_reply_to.into(),
                    ..Default::default()
                })
                .unwrap();
        }
        ctx.provide::<Store>(store);

        let thread = dispatch(
            &ctx,
            &request("getThread", json!({ "account": "a", "uid": "2" }), json!(1)),
        )
        .await
        .result
        .expect("取线程成功");
        assert!(thread["threadId"].as_i64().unwrap() > 0);
        let members = thread["messages"].as_array().unwrap();
        assert_eq!(members.len(), 2);
        // 线程按时间正序——它是要从头读到尾的。
        assert_eq!(members[0]["uid"], "1");
        assert_eq!(members[1]["uid"], "2");

        let status = dispatch(&ctx, &request("getSearchIndexStatus", json!({}), json!(2)))
            .await
            .result
            .expect("读取进度成功");
        assert_eq!(status["pending"], 0);
        assert_eq!(status["done"], true);

        for params in [
            json!({ "account": "a" }),
            json!({ "uid": "1" }),
            json!({ "account": "a", "uid": "1", "unexpected": true }),
            json!({ "account": "a", "threadId": 0 }),
        ] {
            let response = dispatch(&ctx, &request("getThread", params, json!(3))).await;
            assert_eq!(response.error.unwrap().code, INVALID_PARAMS);
        }
        let response = dispatch(
            &ctx,
            &request("getSearchIndexStatus", json!({ "any": 1 }), json!(4)),
        )
        .await;
        assert_eq!(response.error.unwrap().code, INVALID_PARAMS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_message_summaries_filters_account_without_body() {
        let ctx = Context::new();
        let store = Arc::new(Store::open("").unwrap());
        for (account, uid, received_at) in [("a", "1", 1), ("b", "2", 2), ("a", "3", 3)] {
            store
                .insert_message(&MailMessage {
                    account: account.into(),
                    uid: uid.into(),
                    sender: format!("{account}@example.test"),
                    recipient: String::new(),
                    subject: format!("subject {uid}"),
                    body: if uid == "3" {
                        "x".repeat(900 * 1024)
                    } else {
                        "正文不应进入摘要响应".into()
                    },
                    received_at,
                    ..Default::default()
                })
                .unwrap();
        }
        ctx.provide::<Store>(store);

        let response = dispatch(
            &ctx,
            &request(
                "listMessageSummaries",
                json!({ "account": "a", "limit": 1, "offset": 0 }),
                json!(1),
            ),
        )
        .await;
        let result = response.result.expect("成功");
        assert_eq!(result["total"], 2);
        assert_eq!(result["messages"][0]["uid"], "3");
        assert!(result["messages"][0].get("body").is_none());

        for params in [
            json!({ "account": "" }),
            json!({ "account": 1 }),
            json!({ "other": true }),
        ] {
            let response = dispatch(&ctx, &request("listMessageSummaries", params, json!(2))).await;
            assert_eq!(response.error.unwrap().code, INVALID_PARAMS);
        }
    }

    /// ADR-0026：文件夹清单、计数、缺席标记与 `folder` 过滤走同一条协议路径。
    #[tokio::test(flavor = "current_thread")]
    async fn list_folders_reports_roles_counts_and_presence() {
        use crate::folders::FolderRole;

        let ctx = Context::new();
        let store = Arc::new(Store::open("").unwrap());
        store
            .upsert_folder("a", "INBOX", "INBOX", Some(FolderRole::Inbox))
            .unwrap();
        store
            .upsert_folder("a", "&XfJT0ZAB-", "已发送", Some(FolderRole::Sent))
            .unwrap();
        for (uid, folder, seen) in [("1", "INBOX", false), ("2", "&XfJT0ZAB-", true)] {
            store
                .insert_message(&MailMessage {
                    account: "a".into(),
                    uid: uid.into(),
                    sender: "s@t".into(),
                    subject: format!("subject {uid}"),
                    body: "body".into(),
                    received_at: 1,
                    folder: folder.into(),
                    seen,
                    ..Default::default()
                })
                .unwrap();
        }
        store.mark_absent_folders("a", &["INBOX".into()]).unwrap();
        ctx.provide::<Store>(store);

        let response = dispatch(&ctx, &request("listFolders", json!({}), json!(1))).await;
        let folders = response.result.expect("成功")["folders"].clone();
        assert_eq!(folders.as_array().unwrap().len(), 2);
        let sent = folders
            .as_array()
            .unwrap()
            .iter()
            .find(|folder| folder["role"] == "sent")
            .expect("已发送");
        assert_eq!(sent["name"], "&XfJT0ZAB-");
        assert_eq!(sent["display_name"], "已发送");
        assert_eq!(sent["total"], 1);
        assert_eq!(sent["unseen"], 0);
        assert_eq!(sent["present"], false, "不在最近一次 LIST 里");

        // `folder` 过滤走线上原名；省略时与旧行为一致。
        let filtered = dispatch(
            &ctx,
            &request(
                "listMessageSummaries",
                json!({ "account": "a", "folder": "&XfJT0ZAB-" }),
                json!(2),
            ),
        )
        .await
        .result
        .expect("成功");
        assert_eq!(filtered["total"], 1);
        assert_eq!(filtered["messages"][0]["folder_role"], "sent");
        let unfiltered = dispatch(
            &ctx,
            &request("listMessageSummaries", json!({ "account": "a" }), json!(3)),
        )
        .await
        .result
        .expect("成功");
        assert_eq!(unfiltered["total"], 2);

        for params in [json!({ "account": 1 }), json!({ "other": true })] {
            let response = dispatch(&ctx, &request("listFolders", params, json!(4))).await;
            assert_eq!(response.error.unwrap().code, INVALID_PARAMS);
        }
    }

    /// ADR-0027：写入的本地效果、删除的废纸篓策略、队列观测与裁决走同一条协议路径。
    #[tokio::test(flavor = "current_thread")]
    async fn write_path_applies_locally_queues_and_exposes_decisions() {
        let ctx = Context::new();
        let store = Arc::new(Store::open("").unwrap());
        store
            .upsert_folder("a", "INBOX", "INBOX", Some(FolderRole::Inbox))
            .unwrap();
        for uid in ["1", "2"] {
            store
                .insert_message(&MailMessage {
                    account: "a".into(),
                    uid: uid.into(),
                    sender: "s@t".into(),
                    subject: format!("subject {uid}"),
                    body: "body".into(),
                    received_at: 1,
                    folder: "INBOX".into(),
                    ..Default::default()
                })
                .unwrap();
        }
        ctx.provide::<Store>(Arc::clone(&store));

        let marked = dispatch(
            &ctx,
            &request(
                "markMessages",
                json!({
                    "account": "a",
                    "folder": "INBOX",
                    "uids": ["1"],
                    "flag": "seen",
                    "value": true,
                }),
                json!(1),
            ),
        )
        .await
        .result
        .expect("成功");
        assert_eq!(marked["queued"], 1);
        // 本地立刻生效——不等网络。
        let listed = dispatch(
            &ctx,
            &request("listMessageSummaries", json!({ "account": "a" }), json!(2)),
        )
        .await
        .result
        .expect("成功");
        let one = listed["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["uid"] == "1")
            .unwrap();
        assert_eq!(one["seen"], true);

        // 没有废纸篓：删除落 \Deleted，而不是凭空造一个文件夹。
        let deleted = dispatch(
            &ctx,
            &request(
                "deleteMessages",
                json!({ "account": "a", "folder": "INBOX", "uids": ["2"] }),
                json!(3),
            ),
        )
        .await
        .result
        .expect("成功");
        assert_eq!(deleted["mode"], "flag");

        // 有废纸篓：移进去。
        store
            .upsert_folder("a", "Trash", "Trash", Some(FolderRole::Trash))
            .unwrap();
        let deleted = dispatch(
            &ctx,
            &request(
                "deleteMessages",
                json!({ "account": "a", "folder": "INBOX", "uids": ["1"] }),
                json!(4),
            ),
        )
        .await
        .result
        .expect("成功");
        assert_eq!(deleted["mode"], "move");
        assert_eq!(deleted["queued"], 1);

        let ops = dispatch(&ctx, &request("listPendingOps", json!({}), json!(5)))
            .await
            .result
            .expect("成功");
        let ops = ops["ops"].as_array().unwrap().clone();
        assert_eq!(ops.len(), 3, "两个 flag + 一个 move");

        // 未落定的 op 不接受裁决；不存在的 id 报 -32002。
        let move_op = ops.iter().find(|op| op["kind"] == "move").unwrap();
        let resolved = dispatch(
            &ctx,
            &request(
                "resolvePendingOp",
                json!({ "id": move_op["id"], "resolution": "discard" }),
                json!(6),
            ),
        )
        .await
        .result
        .expect("成功");
        assert_eq!(resolved["op"]["state"], "pending");
        let missing = dispatch(
            &ctx,
            &request(
                "resolvePendingOp",
                json!({ "id": 9_999, "resolution": "retry" }),
                json!(7),
            ),
        )
        .await;
        assert_eq!(missing.error.unwrap().code, MESSAGE_NOT_FOUND);

        for (method, params) in [
            ("markMessages", json!({ "account": "a", "folder": "INBOX", "uids": [] })),
            ("markMessages", json!({ "account": "a", "folder": "INBOX", "uids": ["1"] })),
            ("moveMessages", json!({ "account": "a", "folder": "INBOX", "uids": ["1"], "target": "INBOX" })),
            ("deleteMessages", json!({ "account": "a", "folder": "", "uids": ["1"] })),
            ("listPendingOps", json!({ "other": true })),
            ("resolvePendingOp", json!({ "id": 1, "resolution": "maybe" })),
        ] {
            let response = dispatch(&ctx, &request(method, params, json!(8))).await;
            assert_eq!(
                response.error.expect(method).code,
                INVALID_PARAMS,
                "{method} 应拒绝非法参数"
            );
        }
    }

    /// ADR-0028：草稿在内核成型——头继承、主题归一、排除自己、转发带原附件。
    #[tokio::test(flavor = "current_thread")]
    async fn prepare_reply_shapes_the_draft_in_the_kernel() {
        let ctx = Context::new();
        let store = Arc::new(Store::open("").unwrap());
        store
            .upsert_folder("a", "INBOX", "INBOX", Some(FolderRole::Inbox))
            .unwrap();
        store
            .insert_message(&MailMessage {
                account: "a".into(),
                uid: "7".into(),
                sender: "Alice <alice@example.test>".into(),
                recipient: "me@example.test, team@example.test".into(),
                cc: "carol@example.test".into(),
                subject: "Re: RE: 周会纪要".into(),
                body: "第一行\n第二行".into(),
                received_at: 1_646_128_800,
                date_at: Some(1_646_128_800),
                message_id: "parent@example.test".into(),
                references: "<root@example.test>".into(),
                folder: "INBOX".into(),
                attachments: vec![crate::events::MailAttachment {
                    part_index: 2,
                    filename: "报告.pdf".into(),
                    size: 3,
                    ..Default::default()
                }],
                ..Default::default()
            })
            .unwrap();
        ctx.provide::<Store>(store);

        let reply = dispatch(
            &ctx,
            &request(
                "prepareReply",
                json!({ "account": "a", "folder": "INBOX", "uid": "7", "mode": "reply" }),
                json!(1),
            ),
        )
        .await
        .result
        .expect("成功")["draft"]
            .clone();
        assert_eq!(reply["to"], json!(["alice@example.test"]));
        assert_eq!(reply["cc"], json!([]));
        assert_eq!(reply["subject"], "Re: 周会纪要", "前缀不累积");
        assert_eq!(reply["in_reply_to"], "parent@example.test");
        assert_eq!(
            reply["references"],
            "<root@example.test> <parent@example.test>"
        );
        assert!(reply["body"].as_str().unwrap().contains("> 第一行"));

        let reply_all = dispatch(
            &ctx,
            &request(
                "prepareReply",
                json!({ "account": "a", "uid": "7", "mode": "reply_all" }),
                json!(2),
            ),
        )
        .await
        .result
        .expect("成功")["draft"]
            .clone();
        // 没有账户管理器时拿不到别名集合，但去重与「不重复抄送 To」仍然成立。
        let cc = reply_all["cc"].as_array().unwrap();
        assert!(cc.iter().any(|value| value == "team@example.test"));
        assert!(cc.iter().any(|value| value == "carol@example.test"));
        assert!(
            !cc.iter().any(|value| value == "alice@example.test"),
            "已经在 To 里的人不重复抄送"
        );

        let forward = dispatch(
            &ctx,
            &request(
                "prepareReply",
                json!({ "account": "a", "uid": "7", "mode": "forward" }),
                json!(3),
            ),
        )
        .await
        .result
        .expect("成功")["draft"]
            .clone();
        assert_eq!(forward["subject"], "Fwd: 周会纪要");
        assert_eq!(forward["to"], json!([]));
        assert_eq!(forward["in_reply_to"], "", "转发开启新会话");
        assert_eq!(forward["attachments"][0]["part_index"], 2);
        assert!(forward["body"].as_str().unwrap().contains("转发的邮件"));

        for params in [
            json!({ "account": "a", "uid": "7", "mode": "nope" }),
            json!({ "account": "a", "uid": "7" }),
            json!({ "account": "a", "uid": "7", "mode": "reply", "other": 1 }),
        ] {
            let response = dispatch(&ctx, &request("prepareReply", params, json!(4))).await;
            assert_eq!(response.error.unwrap().code, INVALID_PARAMS);
        }
        let missing = dispatch(
            &ctx,
            &request(
                "prepareReply",
                json!({ "account": "a", "uid": "404", "mode": "reply" }),
                json!(5),
            ),
        )
        .await;
        assert_eq!(missing.error.unwrap().code, MESSAGE_NOT_FOUND);
    }

    /// ADR-0048 决策 7：引用块语言 = `locale` 协商，缺省用 `effective` 协商；主题前缀与语言无关。
    #[tokio::test(flavor = "current_thread")]
    async fn prepare_reply_quote_language_follows_locale_then_effective() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::new();
        let store = Arc::new(Store::open("").unwrap());
        store
            .upsert_folder("a", "INBOX", "INBOX", Some(FolderRole::Inbox))
            .unwrap();
        store
            .insert_message(&MailMessage {
                account: "a".into(),
                uid: "7".into(),
                sender: "Alice <alice@example.test>".into(),
                recipient: "me@example.test".into(),
                subject: "答复: Weekly".into(),
                body: "line".into(),
                // < 1971：引用日期回退 RFC 5322，与时区、语言无关，断言才能逐字节。
                received_at: 1234,
                date_at: Some(1234),
                folder: "INBOX".into(),
                ..Default::default()
            })
            .unwrap();
        ctx.provide::<Store>(store);
        let draft = |params: Value| {
            let ctx = ctx.clone();
            async move {
                dispatch(&ctx, &request("prepareReply", params, json!(1)))
                    .await
                    .result
                    .expect("成功")["draft"]
                    .clone()
            }
        };
        let date = crate::compose::rfc5322_date(1234);

        // 没有偏好服务：回落 zh-Hans，与今天逐字节一致。
        let reply = draft(json!({ "account": "a", "uid": "7", "mode": "reply" })).await;
        assert_eq!(
            reply["body"],
            format!("\n\n在 {date}，Alice <alice@example.test> 写道：\n> line")
        );

        // 显式 `locale`：英文引用行；主题前缀仍是 `Re:`，中文前缀照样被剥掉。
        let reply = draft(json!({ "account": "a", "uid": "7", "mode": "reply", "locale": "EN-gb" })).await;
        assert_eq!(reply["subject"], "Re: Weekly");
        assert_eq!(
            reply["body"],
            format!("\n\nOn {date}, Alice <alice@example.test> wrote:\n> line")
        );
        let forward =
            draft(json!({ "account": "a", "uid": "7", "mode": "forward", "locale": "en" })).await;
        assert_eq!(forward["subject"], "Fwd: Weekly");
        assert_eq!(
            forward["body"],
            format!(
                "\n\n---------- Forwarded message ----------\n\
                 From: Alice <alice@example.test>\nDate: {date}\nSubject: 答复: Weekly\n\
                 To: me@example.test\n\nline"
            )
        );

        // 偏好的 `effective` 为英文首选：不带 `locale` 也是英文；`locale` 优先于 `effective`；
        // `null` 等于缺省；内核不支持的语言（含繁体）协商回落中文。
        ctx.provide::<LocalePreference>(Arc::new(LocalePreference::open_with_system(
            dir.path().join(crate::locale_preference::PREFERENCE_FILE),
            || vec!["en-US".into(), "zh-Hans-CN".into()],
        )));
        for (locale, english) in [
            (None, true),
            (Some(Value::Null), true),
            (Some(json!("zh-CN")), false),
            (Some(json!("zh-Hant-TW")), false),
            (Some(json!("ja")), false),
        ] {
            let mut params = json!({ "account": "a", "uid": "7", "mode": "reply" });
            if let Some(locale) = locale.clone() {
                params["locale"] = locale;
            }
            let body = draft(params).await["body"].as_str().unwrap().to_owned();
            assert_eq!(body.contains(" wrote:"), english, "{locale:?}: {body}");
            assert_eq!(body.contains("写道："), !english, "{locale:?}: {body}");
        }

        for locale in [json!(""), json!("not a tag"), json!("x".repeat(36)), json!(5), json!(["en"])] {
            let response = dispatch(
                &ctx,
                &request(
                    "prepareReply",
                    json!({ "account": "a", "uid": "7", "mode": "reply", "locale": locale }),
                    json!(2),
                ),
            )
            .await;
            assert_eq!(response.error.unwrap().code, INVALID_PARAMS, "{locale}");
        }
        assert!(
            !dir.path().join(crate::locale_preference::PREFERENCE_FILE).exists(),
            "prepareReply 只读偏好"
        );
    }

    /// ADR-0034：草稿的参数面也要「拼错就报错」。自动保存每隔几秒发一次，
    /// 一个被静默忽略的字段名意味着用户看着「已保存」而内容其实没存。
    #[tokio::test(flavor = "current_thread")]
    async fn draft_params_reject_unknown_fields_and_shape_errors() {
        let ctx = Context::new();
        ctx.provide::<Store>(Arc::new(Store::open("").unwrap()));

        let id = dispatch(
            &ctx,
            &request("createDraft", json!({ "account": "a", "body": "在写" }), json!(1)),
        )
        .await
        .result
        .expect("新建成功")["draft"]["id"]
            .as_str()
            .unwrap()
            .to_owned();

        for (params, what) in [
            (json!({ "account": "a", "bdoy": "拼错的字段名" }), "createDraft 未知字段"),
            (json!({ "body": "没有账户" }), "createDraft 缺 account"),
            (json!({ "account": "", "body": "空账户" }), "createDraft 空 account"),
        ] {
            let response = dispatch(&ctx, &request("createDraft", params, json!(2))).await;
            assert_eq!(response.error.expect(what).code, INVALID_PARAMS, "{what}");
        }
        for (params, what) in [
            (
                json!({ "id": id, "base_version": 1, "subjcet": "拼错" }),
                "updateDraft 未知字段",
            ),
            (json!({ "id": id, "subject": "没带版本" }), "updateDraft 缺 base_version"),
            (
                json!({ "id": id, "base_version": 0, "subject": "版本从 1 起" }),
                "updateDraft 版本为 0",
            ),
        ] {
            let response = dispatch(&ctx, &request("updateDraft", params, json!(3))).await;
            assert_eq!(response.error.expect(what).code, INVALID_PARAMS, "{what}");
        }

        // 空补丁是合法的一次「确认还在」，但仍然要带对版本号。
        let confirmed = dispatch(
            &ctx,
            &request("updateDraft", json!({ "id": id, "base_version": 1 }), json!(4)),
        )
        .await
        .result
        .expect("空补丁可接受")["draft"]
            .clone();
        assert_eq!(confirmed["body"], "在写");
        assert_eq!(confirmed["version"], 2);

        let missing = dispatch(&ctx, &request("getDraft", json!({ "id": "draft_nope" }), json!(5))).await;
        assert_eq!(missing.error.expect("不存在").code, DRAFT_NOT_FOUND);
    }

    /// ADR-0028 决策 2/3：收件人逐项校验，拼错的字段不被静默忽略。
    #[tokio::test(flavor = "current_thread")]
    async fn send_validates_every_recipient_and_rejects_unknown_keys() {
        let ctx = Context::new();
        for params in [
            // 头注入：即使藏在数组的第二项里也要被逐项挡下。
            json!({
                "from": "me@example.test",
                "to": ["ok@example.test", "victim@example.test\r\nBcc: leak@example.test"],
            }),
            json!({ "from": "me@example.test", "to": ["ok@example.test"], "cc": ["bad addr"] }),
            json!({ "from": "me@example.test", "to": ["ok@example.test"], "bcc": [""] }),
            json!({ "from": "me@example.test", "to": [] }),
            json!({ "from": "me@example.test", "to": "ok@example.test", "subjekt": "拼错了" }),
            json!({ "from": "me@example.test", "to": 42 }),
        ] {
            let response = dispatch(&ctx, &request("send", params.clone(), json!(1))).await;
            assert_eq!(
                response.error.expect("应拒绝").code,
                INVALID_PARAMS,
                "{params}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_method_returns_method_not_found() {
        let ctx = Context::new();
        let resp = dispatch(&ctx, &request("nope", json!({}), json!(9))).await;
        let e = resp.error.expect("错误");
        assert_eq!(e.code, METHOD_NOT_FOUND);
        assert_eq!(resp.id, json!(9));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_jsonrpc_shape_returns_invalid_request() {
        let ctx = Context::new();
        for frame in [
            br#"{}"#.as_slice(),
            br#"{"jsonrpc":"1.0","method":"status","id":7}"#.as_slice(),
            br#"{"jsonrpc":"2.0","method":"status","params":true,"id":7}"#.as_slice(),
        ] {
            let response = parse_request(frame).expect_err("非法请求结构");
            assert_eq!(response.error.unwrap().code, INVALID_REQUEST);
        }

        let response = dispatch(
            &ctx,
            &JsonRpcRequest {
                jsonrpc: "1.0".into(),
                method: "status".into(),
                params: Value::Null,
                id: json!(7),
            },
        )
        .await;
        assert_eq!(response.error.unwrap().code, INVALID_REQUEST);

        let oversized_id = "x".repeat(MAX_REQUEST_ID_BYTES - 1);
        let frame = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "status",
            "id": oversized_id,
        }))
        .unwrap();
        let response = parse_request(&frame).expect_err("过大的 id 不是有效请求 id");
        assert_eq!(response.id, Value::Null);
        assert_eq!(response.error.unwrap().code, INVALID_REQUEST);

        let valid_id = "x".repeat(MAX_REQUEST_ID_BYTES - 2);
        let frame = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "status",
            "params": {},
            "id": valid_id,
        }))
        .unwrap();
        let request = parse_request(&frame).expect("4 KiB id 有效");
        let response = dispatch(&ctx, &request).await;
        let line = response_line(response);
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            value["id"].as_str().unwrap().len(),
            MAX_REQUEST_ID_BYTES - 2
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_messages_without_store_is_unavailable() {
        let ctx = Context::new();
        let invalid = dispatch(&ctx, &request("listMessages", json!([]), json!(1))).await;
        assert_eq!(invalid.error.unwrap().code, INVALID_PARAMS);
        let resp = dispatch(&ctx, &request("listMessages", json!({}), json!(1))).await;
        let e = resp.error.expect("错误");
        assert_eq!(e.code, SERVICE_UNAVAILABLE);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_message_returns_account_scoped_full_detail() {
        let ctx = Context::new();
        let store = Arc::new(Store::open("").unwrap());
        for (account, sender) in [("a", "alice"), ("b", "bob")] {
            store
                .insert_message(&MailMessage {
                    account: account.into(),
                    uid: "same".into(),
                    sender: sender.into(),
                    recipient: String::new(),
                    subject: "subject".into(),
                    body: "line one\nline two\n".into(),
                    received_at: 42,
                    ..Default::default()
                })
                .unwrap();
        }
        ctx.provide::<Store>(store);
        let resp = dispatch(
            &ctx,
            &request(
                "getMessage",
                json!({ "account": "b", "uid": "same" }),
                json!(11),
            ),
        )
        .await;
        assert_eq!(resp.id, json!(11));
        assert!(resp.error.is_none());
        let message = &resp.result.unwrap()["message"];
        assert_eq!(message["account"], "b");
        assert_eq!(message["uid"], "same");
        assert_eq!(message["sender"], "bob");
        assert_eq!(message["subject"], "subject");
        assert_eq!(message["body"], "line one\nline two\n");
        assert_eq!(message["received_at"], 42);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_message_rejects_invalid_identity_params() {
        let ctx = Context::new();
        ctx.provide::<Store>(Arc::new(Store::open("").unwrap()));
        for params in [
            Value::Null,
            json!([]),
            json!({}),
            json!({ "account": null, "uid": "1" }),
            json!({ "account": 1, "uid": "1" }),
            json!({ "account": "", "uid": "1" }),
            json!({ "account": "a", "uid": null }),
            json!({ "account": "a", "uid": 1 }),
            json!({ "account": "a", "uid": "" }),
        ] {
            let resp = dispatch(&ctx, &request("getMessage", params, json!(1))).await;
            assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_message_without_store_is_unavailable() {
        let ctx = Context::new();
        let resp = dispatch(
            &ctx,
            &request(
                "getMessage",
                json!({ "account": "a", "uid": "1" }),
                json!(2),
            ),
        )
        .await;
        assert_eq!(resp.error.unwrap().code, SERVICE_UNAVAILABLE);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_message_missing_returns_message_not_found() {
        let ctx = Context::new();
        ctx.provide::<Store>(Arc::new(Store::open("").unwrap()));
        let resp = dispatch(
            &ctx,
            &request(
                "getMessage",
                json!({ "account": "a", "uid": "1" }),
                json!(3),
            ),
        )
        .await;
        assert_eq!(resp.id, json!(3));
        assert_eq!(resp.result, None);
        let error = resp.error.unwrap();
        assert_eq!(error.code, MESSAGE_NOT_FOUND);
        assert_eq!(error.message, "message not found");
        assert_eq!(error.data, None);
    }

    // —— ADR-0032：附件字节通道与阅读策略 ——

    /// 预置：收件箱与已发送各有一封 uid 相同的信，附件 part 2 内容不同；收件箱那封还带
    /// 一个未落库的超限 part 4 与一个零长 part 5。
    fn seed_attachment_store() -> Arc<Store> {
        let store = Arc::new(Store::open("").unwrap());
        store
            .upsert_folder("a", "INBOX", "收件箱", Some(FolderRole::Inbox))
            .unwrap();
        store
            .upsert_folder("a", "Sent", "已发送", Some(FolderRole::Sent))
            .unwrap();
        let inbox_bytes: Vec<u8> = (0..(MAX_ATTACHMENT_CHUNK_BYTES + 3))
            .map(|index| (index % 251) as u8)
            .collect();
        store
            .insert_message(&MailMessage {
                account: "a".into(),
                uid: "7".into(),
                sender: "Newsletter <News@Example.com>".into(),
                subject: "inbox".into(),
                folder: "INBOX".into(),
                has_attachments: true,
                attachments: vec![
                    crate::events::MailAttachment {
                        part_index: 2,
                        filename: "报告.pdf".into(),
                        mime_type: "application/pdf".into(),
                        size: inbox_bytes.len() as u64,
                        stored: true,
                        content: Some(inbox_bytes),
                        ..Default::default()
                    },
                    crate::events::MailAttachment {
                        part_index: 4,
                        filename: "big.bin".into(),
                        mime_type: "application/octet-stream".into(),
                        size: 3 * 1024 * 1024,
                        stored: false,
                        content: None,
                        ..Default::default()
                    },
                    crate::events::MailAttachment {
                        part_index: 5,
                        filename: "empty.txt".into(),
                        mime_type: "text/plain".into(),
                        size: 0,
                        stored: true,
                        content: Some(Vec::new()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            })
            .unwrap();
        store
            .insert_message(&MailMessage {
                account: "a".into(),
                uid: "7".into(),
                sender: "me@example.org".into(),
                subject: "sent".into(),
                folder: "Sent".into(),
                has_attachments: true,
                attachments: vec![crate::events::MailAttachment {
                    part_index: 2,
                    filename: "sent.txt".into(),
                    mime_type: "text/plain".into(),
                    size: 4,
                    stored: true,
                    content: Some(b"sent".to_vec()),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .unwrap();
        store
    }

    fn read_params(folder: &str, part: u32, offset: Option<u64>, length: Option<u64>) -> Value {
        let mut params = json!({ "account": "a", "folder": folder, "uid": "7", "part_index": part });
        if let Some(offset) = offset {
            params["offset"] = json!(offset);
        }
        if let Some(length) = length {
            params["length"] = json!(length);
        }
        params
    }

    fn decode_chunk(result: &Value) -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(result["data"].as_str().unwrap())
            .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_attachment_streams_bounded_chunks_that_fit_one_frame() {
        let ctx = Context::new();
        ctx.provide::<Store>(seed_attachment_store());
        let expected: Vec<u8> = (0..(MAX_ATTACHMENT_CHUNK_BYTES + 3))
            .map(|index| (index % 251) as u8)
            .collect();

        // 第一块：缺省 length = 上限，整帧落在响应上限内。
        let response = dispatch(&ctx, &request("readAttachment", read_params("INBOX", 2, None, None), json!(1))).await;
        let frame = serde_json::to_vec(&response).unwrap();
        assert!(frame.len() <= MAX_RESPONSE_BYTES, "最大块的整帧 {} 字节超过上限", frame.len());
        let result = response.result.expect("成功");
        assert_eq!(result["offset"], 0);
        assert_eq!(result["length"], MAX_ATTACHMENT_CHUNK_BYTES as u64);
        assert_eq!(result["eof"], false);
        assert_eq!(result["attachment"]["part_index"], 2);
        assert_eq!(result["attachment"]["filename"], "报告.pdf");
        assert_eq!(result["attachment"]["size"], expected.len() as u64);
        assert_eq!(result["attachment"]["stored"], true);
        assert!(result["attachment"].get("content").is_none(), "字节只在 data 里");
        let mut collected = decode_chunk(&result);

        // 第二块：客户端按已收字节推进 offset，收到 eof。
        let result = dispatch(
            &ctx,
            &request("readAttachment", read_params("INBOX", 2, Some(collected.len() as u64), None), json!(2)),
        )
        .await
        .result
        .expect("成功");
        assert_eq!(result["length"], 3);
        assert_eq!(result["eof"], true);
        collected.extend(decode_chunk(&result));
        assert_eq!(collected, expected, "跨块拼装后字节逐一相等");

        // offset == size：空块 + eof，循环终止条件不必特判零长。
        let result = dispatch(
            &ctx,
            &request("readAttachment", read_params("INBOX", 2, Some(expected.len() as u64), None), json!(3)),
        )
        .await
        .result
        .expect("成功");
        assert_eq!(result["length"], 0);
        assert_eq!(result["data"], "");
        assert_eq!(result["eof"], true);

        // 零长附件同理。
        let result = dispatch(&ctx, &request("readAttachment", read_params("INBOX", 5, None, None), json!(4)))
            .await
            .result
            .expect("成功");
        assert_eq!(result["eof"], true);
        assert_eq!(result["attachment"]["size"], 0);

        // 自定义 length 精确切片。
        let result = dispatch(&ctx, &request("readAttachment", read_params("INBOX", 2, Some(10), Some(5)), json!(5)))
            .await
            .result
            .expect("成功");
        assert_eq!(decode_chunk(&result), expected[10..15].to_vec());

        // 决策 2：同 uid 的已发送那封是另一封信，读到的是它自己的字节。
        let result = dispatch(&ctx, &request("readAttachment", read_params("Sent", 2, None, None), json!(6)))
            .await
            .result
            .expect("成功");
        assert_eq!(decode_chunk(&result), b"sent".to_vec());
        assert_eq!(result["attachment"]["filename"], "sent.txt");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_attachment_distinguishes_missing_message_part_and_bytes() {
        let ctx = Context::new();
        ctx.provide::<Store>(seed_attachment_store());
        let code_of = |params: Value| async {
            dispatch(&ctx, &request("readAttachment", params, json!(1)))
                .await
                .error
                .expect("应失败")
                .code
        };
        // 决策 3：三种「拿不到」各有其名。
        assert_eq!(code_of(json!({ "account": "nobody", "folder": "INBOX", "uid": "7", "part_index": 2 })).await, MESSAGE_NOT_FOUND);
        assert_eq!(code_of(json!({ "account": "a", "folder": "Archive", "uid": "7", "part_index": 2 })).await, MESSAGE_NOT_FOUND);
        assert_eq!(code_of(json!({ "account": "a", "folder": "INBOX", "uid": "8", "part_index": 2 })).await, MESSAGE_NOT_FOUND);
        assert_eq!(code_of(read_params("INBOX", 3, None, None)).await, ATTACHMENT_NOT_FOUND);
        assert_eq!(code_of(read_params("INBOX", 4, None, None)).await, ATTACHMENT_NOT_STORED);
        // 已发送那封没有 part 4/5——part 是按封的。
        assert_eq!(code_of(read_params("Sent", 4, None, None)).await, ATTACHMENT_NOT_FOUND);

        // 参数门：folder 必填、越界 offset、非法 length、非法 part_index。
        for params in [
            json!({ "account": "a", "uid": "7", "part_index": 2 }),
            json!({ "account": "a", "folder": "", "uid": "7", "part_index": 2 }),
            json!({ "account": "a", "folder": "INBOX", "uid": "7" }),
            json!({ "account": "a", "folder": "INBOX", "uid": "7", "part_index": -1 }),
            json!({ "account": "a", "folder": "INBOX", "uid": "7", "part_index": 2, "offset": -1 }),
            read_params("INBOX", 2, Some(MAX_ATTACHMENT_CHUNK_BYTES as u64 + 4), None),
            read_params("INBOX", 2, None, Some(0)),
            read_params("INBOX", 2, None, Some(MAX_ATTACHMENT_CHUNK_BYTES as u64 + 1)),
            Value::Null,
        ] {
            assert_eq!(code_of(params.clone()).await, INVALID_PARAMS, "{params}");
        }

        // 乐观删除中的信对字节通道同样不可见（ADR-0027 决策 5）。
        let store = ctx.get::<Store>().unwrap();
        assert_eq!(
            store
                .queue_move_ops("a", "INBOX", &["7".to_string()], "Archive", 1)
                .unwrap(),
            1
        );
        assert_eq!(code_of(read_params("INBOX", 2, None, None)).await, MESSAGE_NOT_FOUND);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn get_message_carries_stored_flags_and_the_remote_content_decision() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::new();
        ctx.provide::<Store>(seed_attachment_store());
        ctx.provide::<ReadingPolicy>(Arc::new(ReadingPolicy::open(
            dir.path().join(crate::reading_policy::POLICY_FILE),
        )));
        let get = || async {
            dispatch(&ctx, &request("getMessage", json!({ "account": "a", "folder": "INBOX", "uid": "7" }), json!(1)))
                .await
                .result
                .expect("成功")
        };

        let result = get().await;
        let attachments = result["message"]["attachments"].as_array().unwrap();
        assert_eq!(attachments.len(), 3);
        assert_eq!(attachments[0]["stored"], true);
        assert_eq!(attachments[1]["stored"], false, "超限 part 如实说没存");
        assert_eq!(attachments[2]["stored"], true);
        assert_eq!(result["remote_content"], json!({ "decision": "block", "reason": "default" }));

        // 信任地址 → 翻转；大小写不敏感。
        dispatch(&ctx, &request("setReadingPolicy", json!({ "trust": ["news@example.com"] }), json!(2)))
            .await
            .result
            .expect("成功");
        assert_eq!(get().await["remote_content"], json!({ "decision": "allow", "reason": "trusted_address" }));

        dispatch(&ctx, &request("setReadingPolicy", json!({ "untrust": ["news@example.com"], "trust": ["example.com"] }), json!(3)))
            .await
            .result
            .expect("成功");
        assert_eq!(get().await["remote_content"], json!({ "decision": "allow", "reason": "trusted_domain" }));

        dispatch(&ctx, &request("setReadingPolicy", json!({ "remote_content": "allow" }), json!(4)))
            .await
            .result
            .expect("成功");
        assert_eq!(get().await["remote_content"], json!({ "decision": "allow", "reason": "allow_all" }));

        // 没装配策略服务时结论与缺省一致，键仍在。
        let bare = Context::new();
        bare.provide::<Store>(seed_attachment_store());
        let result = dispatch(&bare, &request("getMessage", json!({ "account": "a", "folder": "INBOX", "uid": "7" }), json!(5)))
            .await
            .result
            .expect("成功");
        assert_eq!(result["remote_content"], json!({ "decision": "block", "reason": "default" }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reading_policy_patch_is_validated_as_a_whole_and_reports_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::new();
        ctx.provide::<ReadingPolicy>(Arc::new(ReadingPolicy::open(
            dir.path().join(crate::reading_policy::POLICY_FILE),
        )));
        let result = dispatch(&ctx, &request("getReadingPolicy", json!({}), json!(1)))
            .await
            .result
            .expect("成功");
        assert_eq!(result["policy"]["remote_content"], "block");
        assert_eq!(result["policy"]["trusted_senders"], json!([]));
        assert_eq!(result["persisted"], true);

        for params in [
            json!({}),
            json!({ "remote_content": "maybe" }),
            json!({ "trust": ["not an address"] }),
            json!({ "trust": ["ok@example.com", "bad"] }),
            json!({ "trust": ["x@example.com"], "untrust": ["X@example.com"] }),
            json!({ "trusted": ["x@example.com"] }),
            json!({ "trust": "x@example.com" }),
            Value::Null,
        ] {
            let error = dispatch(&ctx, &request("setReadingPolicy", params.clone(), json!(2)))
                .await
                .error
                .unwrap_or_else(|| panic!("{params} 应被拒绝"));
            assert_eq!(error.code, INVALID_PARAMS, "{params}");
        }
        // 整包拒绝：名单仍为空，文件未创建。
        assert_eq!(
            dispatch(&ctx, &request("getReadingPolicy", json!({}), json!(3)))
                .await
                .result
                .unwrap()["policy"]["trusted_senders"],
            json!([])
        );
        assert!(!dir.path().join(crate::reading_policy::POLICY_FILE).exists());

        let result = dispatch(&ctx, &request("setReadingPolicy", json!({ "trust": ["Alice@Example.com"], "remote_content": "block" }), json!(4)))
            .await
            .result
            .expect("成功");
        assert_eq!(result["policy"]["trusted_senders"][0]["value"], "alice@example.com");
        assert_eq!(result["policy"]["trusted_senders"][0]["kind"], "address");
        assert!(result["policy"]["trusted_senders"][0]["added_at"].as_u64().unwrap() > 0);
        assert_eq!(result["persisted"], true);

        // 服务未装配：两个方法都 -32001。
        let bare = Context::new();
        for method in ["getReadingPolicy", "setReadingPolicy"] {
            let error = dispatch(&bare, &request(method, json!({ "remote_content": "block" }), json!(5)))
                .await
                .error
                .unwrap();
            assert_eq!(error.code, SERVICE_UNAVAILABLE, "{method}");
        }
    }

    fn locale_ctx(dir: &std::path::Path) -> Context {
        let ctx = Context::new();
        ctx.provide::<LocalePreference>(Arc::new(LocalePreference::open_with_system(
            dir.join(crate::locale_preference::PREFERENCE_FILE),
            || vec!["en-US".into(), "zh-Hans-CN".into()],
        )));
        ctx
    }

    #[tokio::test(flavor = "current_thread")]
    async fn locale_preference_is_validated_as_a_whole_and_reports_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(crate::locale_preference::PREFERENCE_FILE);
        let ctx = locale_ctx(dir.path());
        let result = dispatch(&ctx, &request("getLocalePreference", json!({}), json!(1)))
            .await
            .result
            .expect("成功");
        assert_eq!(
            result,
            json!({
                "preference": { "mode": "system", "languages": [] },
                "effective": ["en-US", "zh-Hans-CN"],
                "persisted": true
            })
        );
        assert!(!path.exists(), "只读从不落盘");
        let error = dispatch(&ctx, &request("getLocalePreference", json!({ "x": 1 }), json!(1)))
            .await
            .error
            .unwrap();
        assert_eq!(error.code, INVALID_PARAMS);

        let nine = json!(["en", "fr", "de", "ja", "ko", "es", "it", "pt", "ru"]);
        for params in [
            json!({}),
            json!({ "mode": "auto" }),
            json!({ "mode": "explicit" }),
            json!({ "mode": "explicit", "languages": [] }),
            json!({ "mode": "explicit", "languages": ["en", "not a tag"] }),
            json!({ "mode": "explicit", "languages": ["en_US"] }),
            json!({ "mode": "explicit", "languages": ["en-abcdefgh-abcdefgh-abcdefgh-abcdef"] }),
            json!({ "mode": "explicit", "languages": nine }),
            json!({ "mode": "explicit", "languages": "en" }),
            json!({ "mode": "explicit", "languages": [1] }),
            json!({ "mode": "system", "languages": ["en"] }),
            json!({ "mode": "system", "languages": [] }),
            json!({ "mode": "system", "extra": true }),
            json!({ "mode": "explicit", "languages": ["en"], "lang": "en" }),
            Value::Null,
        ] {
            let error = dispatch(&ctx, &request("setLocalePreference", params.clone(), json!(2)))
                .await
                .error
                .unwrap_or_else(|| panic!("{params} 应被拒绝"));
            assert_eq!(error.code, INVALID_PARAMS, "{params}");
        }
        assert!(!path.exists(), "整包拒绝不落盘");

        let result = dispatch(
            &ctx,
            &request("setLocalePreference", json!({ "mode": "explicit", "languages": ["EN", "zh-hans", "en"] }), json!(3)),
        )
        .await
        .result
        .expect("成功");
        assert_eq!(
            result,
            json!({
                "preference": { "mode": "explicit", "languages": ["en", "zh-Hans"] },
                "effective": ["en", "zh-Hans"],
                "persisted": true
            })
        );
        assert!(path.exists());
        assert_eq!(
            dispatch(&ctx, &request("getLocalePreference", json!({}), json!(4)))
                .await
                .result
                .unwrap(),
            result,
            "成功应答与 get 同形"
        );

        // 服务未装配：合法参数 -32001；非法参数仍先 -32602。
        let bare = Context::new();
        for (method, params) in [
            ("getLocalePreference", json!({})),
            ("setLocalePreference", json!({ "mode": "system" })),
        ] {
            let error = dispatch(&bare, &request(method, params, json!(5))).await.error.unwrap();
            assert_eq!(error.code, SERVICE_UNAVAILABLE, "{method}");
        }
        let error = dispatch(&bare, &request("setLocalePreference", json!({ "mode": "bad" }), json!(6)))
            .await
            .error
            .unwrap();
        assert_eq!(error.code, INVALID_PARAMS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_locale_preference_reads_default_and_refuses_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(crate::locale_preference::PREFERENCE_FILE);
        std::fs::write(&path, "format_version = 9\nmode = \"system\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let ctx = locale_ctx(dir.path());
        let result = dispatch(&ctx, &request("getLocalePreference", json!({}), json!(1)))
            .await
            .result
            .unwrap();
        assert_eq!(result["preference"]["mode"], "system");
        assert_eq!(result["effective"], json!(["en-US", "zh-Hans-CN"]));
        assert_eq!(result["persisted"], false);
        let error = dispatch(
            &ctx,
            &request("setLocalePreference", json!({ "mode": "explicit", "languages": ["en"] }), json!(2)),
        )
        .await
        .error
        .unwrap();
        assert_eq!(error.code, SERVICE_UNAVAILABLE);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "format_version = 9\nmode = \"system\"\n",
            "停摆文件不得被覆盖"
        );
    }

    /// `setLocalePreference` 成功后，所有流式连接（含发起者与未认证会话）都收到 `preferences/locale`；
    /// `effective` 没变的写入不再推送。
    #[tokio::test(flavor = "current_thread")]
    async fn locale_preference_push_reaches_every_streaming_connection() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = locale_ctx(dir.path());

        let (watcher, server) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(server);
        let watcher_task = tokio::spawn(serve(ctx.clone(), BufReader::new(read), write));
        let (setter, server) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(server);
        let setter_task = tokio::spawn(serve(ctx.clone(), BufReader::new(read), write));
        tokio::task::yield_now().await;

        let (setter_read, mut setter_write) = tokio::io::split(setter);
        let mut setter_lines = BufReader::new(setter_read).lines();
        let mut watcher_lines = BufReader::new(watcher).lines();
        async fn next<R: AsyncBufRead + Unpin>(lines: &mut tokio::io::Lines<R>) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(1), lines.next_line())
                .await
                .expect("帧应到达")
                .unwrap()
                .unwrap();
            serde_json::from_str(&line).unwrap()
        }

        let set = |id: u64, params: Value| {
            format!(
                "{}\n",
                json!({ "jsonrpc": "2.0", "id": id, "method": "setLocalePreference", "params": params })
            )
        };
        setter_write
            .write_all(set(1, json!({ "mode": "explicit", "languages": ["fr", "en"] })).as_bytes())
            .await
            .unwrap();

        let expected = json!({
            "jsonrpc": "2.0",
            "method": "preferences/locale",
            "params": { "effective": ["fr", "en"] }
        });
        assert_eq!(next(&mut watcher_lines).await, expected);
        let mut setter_frames = [next(&mut setter_lines).await, next(&mut setter_lines).await];
        setter_frames.sort_by_key(|frame| frame.get("id").is_some());
        assert_eq!(setter_frames[0], expected);
        assert_eq!(setter_frames[1]["id"], 1);
        assert_eq!(setter_frames[1]["result"]["effective"], json!(["fr", "en"]));

        // 同一结果再写一次：应答照常，不推送；随后切回 system 再推一次。
        setter_write
            .write_all(set(2, json!({ "mode": "explicit", "languages": ["FR", "en"] })).as_bytes())
            .await
            .unwrap();
        let reply = next(&mut setter_lines).await;
        assert_eq!(reply["id"], 2, "effective 未变不推送: {reply}");
        setter_write
            .write_all(set(3, json!({ "mode": "system" })).as_bytes())
            .await
            .unwrap();
        assert_eq!(
            next(&mut watcher_lines).await["params"],
            json!({ "effective": ["en-US", "zh-Hans-CN"] }),
            "watcher 的下一帧就是切回 system 的推送"
        );

        drop(setter_write);
        drop(setter_lines);
        drop(watcher_lines);
        let _ = tokio::time::timeout(Duration::from_secs(1), setter_task).await;
        let _ = tokio::time::timeout(Duration::from_secs(1), watcher_task).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oversized_get_message_is_bounded_not_truncated() {
        let ctx = Context::new();
        let store = Arc::new(Store::open("").unwrap());
        store
            .insert_message(&MailMessage {
                account: "a".into(),
                uid: "1".into(),
                sender: "s".into(),
                recipient: String::new(),
                subject: "s".into(),
                body: "x".repeat(MAX_RESPONSE_BYTES),
                received_at: 1,
                ..Default::default()
            })
            .unwrap();
        ctx.provide::<Store>(store);
        let resp = dispatch(
            &ctx,
            &request(
                "getMessage",
                json!({ "account": "a", "uid": "1" }),
                json!(4),
            ),
        )
        .await;
        let line = response_line(resp);
        assert!(line.len() <= MAX_RESPONSE_BYTES);
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], 4);
        assert_eq!(value["error"]["code"], INTERNAL_ERROR);
        assert!(value.get("result").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_without_transport_is_unavailable() {
        let ctx = Context::new();
        let invalid = dispatch(&ctx, &request("send", json!([]), json!(1))).await;
        assert_eq!(invalid.error.unwrap().code, INVALID_PARAMS);
        let resp = dispatch(
            &ctx,
            &request(
                "send",
                json!({ "from": "a@test.local", "to": "b@test.local" }),
                json!(2),
            ),
        )
        .await;
        let e = resp.error.expect("错误");
        assert_eq!(e.code, SERVICE_UNAVAILABLE);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_validates_params_before_connect() {
        let ctx = Context::new();
        ctx.provide::<TransportConfig>(Arc::new(TransportConfig::default()));
        ctx.plugin(Arc::new(TransportPlugin));
        // 缺 `to` → invalid params（在连网前短路）
        let resp = dispatch(&ctx, &request("send", json!({ "from": "a@b" }), json!(3))).await;
        let e = resp.error.expect("错误");
        assert_eq!(e.code, INVALID_PARAMS);

        let resp = dispatch(
            &ctx,
            &request(
                "send",
                json!({ "from": "a@b\r\nRCPT TO:<x@y>", "to": "b@c" }),
                json!(4),
            ),
        )
        .await;
        let e = resp.error.expect("拒绝 SMTP 注入");
        assert_eq!(e.code, INVALID_PARAMS);

        for params in [
            json!([]),
            json!({ "from": 1, "to": "b@c" }),
            json!({ "from": "a@b", "to": false }),
            json!({ "from": "a@b", "to": "b@c", "subject": 1 }),
            json!({ "from": "a@b", "to": "b@c", "body": false }),
            json!({ "from": format!("{}@b", "a".repeat(320)), "to": "b@c" }),
            json!({ "from": "a@b", "to": "b@c", "subject": "x".repeat(999) }),
            json!({ "from": "a@b", "to": "b@c", "body": "x".repeat(512 * 1024 + 1) }),
        ] {
            let response = dispatch(&ctx, &request("send", params, json!(5))).await;
            assert_eq!(response.error.unwrap().code, INVALID_PARAMS);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oversized_frame_is_rejected_without_unbounded_buffering() {
        let input = vec![b'x'; MAX_FRAME_BYTES + 1];
        let mut reader = BufReader::new(input.as_slice());
        assert!(matches!(
            read_frame(&mut reader).await.unwrap(),
            Some(Err(()))
        ));
    }

    #[test]
    fn message_notification_caps_remote_text() {
        let line = mail_notification(MailMessage {
            account: "a".into(),
            uid: "1".into(),
            sender: "s".into(),
            recipient: "r".repeat(MAX_PUSH_TEXT_BYTES + 1),
            subject: "x".repeat(MAX_PUSH_TEXT_BYTES + 1),
            body: "中".repeat(MAX_PUSH_BODY_BYTES),
            received_at: 0,
            // ADR-0025：通知是「有新邮件」的信号，不是持久队列。
            body_html: "<p>".repeat(MAX_PUSH_BODY_BYTES),
            attachments: vec![crate::events::MailAttachment {
                part_index: 1,
                filename: "doc.pdf".into(),
                size: 9,
                ..Default::default()
            }],
            ..Default::default()
        });
        let value: Value = serde_json::from_str(&line).unwrap();
        assert!(value["params"]["recipient"].as_str().unwrap().len() <= MAX_PUSH_TEXT_BYTES);
        assert!(value["params"]["subject"].as_str().unwrap().len() <= MAX_PUSH_TEXT_BYTES);
        assert!(value["params"]["body"].as_str().unwrap().len() <= MAX_PUSH_BODY_BYTES);
        assert!(
            value["params"].get("body_html").is_none(),
            "推送帧不得携带 HTML 原文"
        );
        assert!(
            value["params"].get("attachments").is_none(),
            "推送帧不得携带附件"
        );
        assert!(line.len() <= MAX_FRAME_BYTES, "推送帧必须落在单帧上限内");
    }

    #[test]
    fn oversized_response_becomes_bounded_error() {
        let line = response_line(JsonRpcResponse::ok(
            json!(7),
            json!({ "body": "x".repeat(MAX_RESPONSE_BYTES) }),
        ));
        assert!(line.len() <= MAX_RESPONSE_BYTES);
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], 7);
        assert_eq!(value["error"]["code"], INTERNAL_ERROR);
    }

    #[test]
    fn notification_line_has_no_id() {
        let line = notification("message/received", json!({ "a": 1 }));
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["method"], "message/received");
        assert!(v.get("id").is_none(), "通知无 id: {line}");
        assert_eq!(v["params"]["a"], 1);
    }

    #[test]
    fn frontend_intent_push_reaches_only_the_exact_frontend_principal() {
        let line = frontend_intent_notification(
            &Principal::Frontend(maild_brand::BUNDLED_FRONTEND_ID.into()),
            maild_brand::BUNDLED_FRONTEND_ID,
        )
        .expect("目标前端收到唤醒");
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["method"], "frontend/intent");
        assert_eq!(v["params"], json!({ "frontend": maild_brand::BUNDLED_FRONTEND_ID }));
        assert!(v.get("id").is_none());
        for other in [
            Principal::User,
            Principal::Plugin(maild_brand::BUNDLED_FRONTEND_ID.into()),
            Principal::Frontend("org.example.reader".into()),
            Principal::Frontend(format!("{}.evil", maild_brand::BUNDLED_FRONTEND_ID)),
        ] {
            assert_eq!(frontend_intent_notification(&other, maild_brand::BUNDLED_FRONTEND_ID), None, "{other:?}");
        }
    }

    /// 未认证（user）会话订阅了推送，但 `frontend/intent` 不会出现在它的流里。
    #[tokio::test(flavor = "current_thread")]
    async fn user_sessions_never_see_frontend_intent_pushes() {
        let ctx = Context::new();
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(server);
        let task = tokio::spawn(serve(ctx.clone(), BufReader::new(read), write));
        tokio::task::yield_now().await;

        ctx.emit::<FrontendIntentQueued>(maild_brand::BUNDLED_FRONTEND_ID.into());
        ctx.emit::<AccountState>(AccountStateInfo::plain(
            "marker",
            crate::events::AccountStateKind::Online,
        ));
        let mut lines = BufReader::new(client).lines();
        let first = tokio::time::timeout(Duration::from_secs(1), lines.next_line())
            .await
            .expect("推送应到达")
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(v["method"], "account/state", "user 会话不该先收到意图唤醒: {first}");
        drop(lines);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }

    #[test]
    fn api_plugin_requires_tokio_runtime() {
        // 无 tokio 运行时（普通 #[test]）→ apply 失败 → fiber 进入 Failed。
        let ctx = Context::new();
        ctx.provide::<ApiConfig>(Arc::new(ApiConfig::default()));
        let handle = ctx.plugin(Arc::new(ApiPlugin));
        assert_eq!(handle.state(), FiberState::Failed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn api_plugin_rejects_non_loopback_http() {
        let ctx = Context::new();
        ctx.provide::<ApiConfig>(Arc::new(ApiConfig {
            transport: "http".into(),
            listen: "0.0.0.0:7394".into(),
            bearer_token: Some("a".repeat(MIN_BEARER_TOKEN_BYTES)),
        }));
        let handle = ctx.plugin(Arc::new(ApiPlugin));
        assert_eq!(handle.state(), FiberState::Failed, "非回环地址 → apply 失败");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn api_plugin_rejects_unknown_transport() {
        let ctx = Context::new();
        ctx.provide::<ApiConfig>(Arc::new(ApiConfig {
            transport: "websocket".into(),
            ..ApiConfig::default()
        }));
        let handle = ctx.plugin(Arc::new(ApiPlugin));
        assert_eq!(handle.state(), FiberState::Failed, "未知传输 → apply 失败");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ipc_shutdown_aborts_blocked_writer_and_joins_session() {
        use tokio::io::AsyncReadExt as _;

        let ctx = Context::new();
        let (mut client, server) = tokio::io::duplex(1);
        let mut sessions = StreamSessions::new();
        spawn_stream_session(&mut sessions, server, &ctx, RequestOrigin::UnixSocket);
        tokio::task::yield_now().await;

        ctx.emit::<MailFiltered>(MailMessage {
            account: "blocked-writer".into(),
            uid: "1".into(),
            sender: "sender@example.test".into(),
            recipient: String::new(),
            subject: "shutdown".into(),
            body: "x".repeat(1024),
            received_at: 1,
            ..Default::default()
        });
        tokio::task::yield_now().await;

        tokio::time::timeout(Duration::from_secs(3), close_stream_sessions(&mut sessions))
            .await
            .expect("blocked writer shutdown must be bounded");
        assert!(
            sessions.tasks.is_empty(),
            "all accepted sessions are joined"
        );

        let mut remaining = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut remaining))
            .await
            .expect("client must observe EOF after session shutdown")
            .expect("read session EOF");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writer_failure_ends_session_without_repolling_join_handle() {
        let ctx = Context::new();
        let (_client, server) = tokio::io::duplex(64);
        let (read, _) = tokio::io::split(server);
        let task = tokio::spawn(serve(ctx.clone(), BufReader::new(read), FailingWriter));
        tokio::task::yield_now().await;

        ctx.emit::<MailFiltered>(MailMessage {
            account: "writer-failure".into(),
            uid: "1".into(),
            sender: "sender@example.test".into(),
            recipient: String::new(),
            subject: "closed peer".into(),
            body: "body".into(),
            received_at: 1,
            ..Default::default()
        });

        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("writer failure must end the session")
            .expect("session must not panic");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ipc_session_count_is_bounded() {
        let ctx = Context::new();
        let mut sessions = StreamSessions::new();
        let mut clients = Vec::new();
        for _ in 0..(MAX_STREAM_SESSIONS + 8) {
            let (client, server) = tokio::io::duplex(64);
            clients.push(client);
            spawn_stream_session(&mut sessions, server, &ctx, RequestOrigin::UnixSocket);
        }
        assert_eq!(sessions.tasks.len(), MAX_STREAM_SESSIONS);
        close_stream_sessions(&mut sessions).await;
        assert!(sessions.tasks.is_empty());
        drop(clients);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn unix_plugin_owns_listener_and_removes_its_socket() {
        use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = Path::new("/tmp").join(format!(
            "maild-api-plugin-dir-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("api.sock");
        let cfg = ApiConfig {
            transport: "unix".into(),
            listen: path.to_string_lossy().into_owned(),
            bearer_token: None,
        };

        let ctx = Context::new();
        ctx.provide::<ApiConfig>(Arc::new(cfg.clone()));
        let handle = ctx.plugin(Arc::new(ApiPlugin));
        assert_eq!(handle.state(), FiberState::Active);
        let metadata = std::fs::metadata(&path).expect("socket 文件已创建");
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

        let other = Context::new();
        other.provide::<ApiConfig>(Arc::new(cfg));
        let duplicate = other.plugin(Arc::new(ApiPlugin));
        assert_eq!(duplicate.state(), FiberState::Failed, "重复监听必须同步失败");
        assert!(path.exists(), "失败的第二个实例不能删除现有 socket");

        handle.dispose().await;
        assert!(!path.exists(), "卸载后删除自己创建的 socket");
        std::fs::remove_dir(directory).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn unix_plugin_rejects_stale_socket_without_unlinking() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::net::UnixListener as StdUnixListener;

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = Path::new("/tmp").join(format!(
            "maild-api-stale-dir-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("api.sock");
        let stale = StdUnixListener::bind(&path).expect("创建 stale socket");
        drop(stale);
        assert!(path.exists());

        let ctx = Context::new();
        ctx.provide::<ApiConfig>(Arc::new(ApiConfig {
            transport: "unix".into(),
            listen: path.to_string_lossy().into_owned(),
            bearer_token: None,
        }));
        let handle = ctx.plugin(Arc::new(ApiPlugin));
        assert_eq!(
            handle.state(),
            FiberState::Failed,
            "stale socket 不应自动删除"
        );
        assert!(path.exists(), "拒绝 stale socket 时不得删除现有路径");
        handle.dispose().await;
        assert!(path.exists());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_config_requires_absolute_existing_parent_and_bounded_path() {
        use std::os::unix::fs::PermissionsExt as _;

        let mut cfg = ApiConfig {
            transport: "unix".into(),
            listen: "relative.sock".into(),
            bearer_token: None,
        };
        assert!(validate_api_config(&cfg).unwrap_err().contains("绝对路径"));

        let directory = Path::new("/tmp").join(format!(
            "maild-api-config-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        cfg.listen = directory
            .join("maild-valid.sock")
            .to_string_lossy()
            .into_owned();
        assert!(validate_api_config(&cfg).is_ok());

        let parent = Path::new(&cfg.listen).parent().unwrap();
        std::fs::remove_dir(parent).unwrap();

        let insecure_directory = Path::new("/tmp").join(format!(
            "maild-api-insecure-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&insecure_directory).unwrap();
        std::fs::set_permissions(&insecure_directory, std::fs::Permissions::from_mode(0o755))
            .unwrap();
        cfg.listen = insecure_directory
            .join("maild-insecure.sock")
            .to_string_lossy()
            .into_owned();
        assert!(validate_api_config(&cfg)
            .unwrap_err()
            .contains("当前用户独占"));
        std::fs::remove_dir(insecure_directory).unwrap();

        let unsafe_ancestor = Path::new("/tmp").join(format!(
            "maild-api-unsafe-ancestor-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let private_leaf = unsafe_ancestor.join("private");
        std::fs::create_dir_all(&private_leaf).unwrap();
        std::fs::set_permissions(&unsafe_ancestor, std::fs::Permissions::from_mode(0o770)).unwrap();
        std::fs::set_permissions(&private_leaf, std::fs::Permissions::from_mode(0o700)).unwrap();
        cfg.listen = private_leaf
            .join(maild_brand::SOCKET_FILE_NAME)
            .to_string_lossy()
            .into_owned();
        assert!(validate_api_config(&cfg)
            .unwrap_err()
            .contains("路径祖先不能被任意用户写入"));
        std::fs::remove_dir(private_leaf).unwrap();
        std::fs::remove_dir(unsafe_ancestor).unwrap();

        cfg.listen = format!("/{}", "x".repeat(MAX_UNIX_SOCKET_PATH_BYTES + 1));
        assert!(validate_api_config(&cfg).unwrap_err().contains("路径过长"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_transport_auth_limits_and_notifications() {
        let ctx = Context::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(serve_http(
            listener,
            ctx,
            "a".repeat(MIN_BEARER_TOKEN_BYTES),
            async move {
                let _ = shutdown_rx.await;
            },
        ));
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/rpc");

        let response = client
            .post(&url)
            .json(&json!({ "jsonrpc": "2.0", "method": "status", "id": 1 }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = client
            .post(&url)
            .bearer_auth("a".repeat(MIN_BEARER_TOKEN_BYTES))
            .json(&json!({ "jsonrpc": "2.0", "method": "status", "id": 1 }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["result"]["message_count"], 0);

        let response = client
            .post(&url)
            .bearer_auth("a".repeat(MIN_BEARER_TOKEN_BYTES))
            .json(&json!({ "jsonrpc": "2.0", "method": "status" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = client
            .post(&url)
            .bearer_auth("a".repeat(MIN_BEARER_TOKEN_BYTES))
            .body(vec![b'x'; MAX_FRAME_BYTES + 1])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let response = client
            .post(&url)
            .bearer_auth("a".repeat(MIN_BEARER_TOKEN_BYTES))
            .body("{")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["code"], PARSE_ERROR);

        let _ = shutdown_tx.send(());
        task.await.unwrap().unwrap();
    }

    #[test]
    fn http_config_rejects_non_loopback_listener() {
        let cfg = ApiConfig {
            transport: "http".into(),
            listen: "0.0.0.0:7394".into(),
            bearer_token: Some("a".repeat(MIN_BEARER_TOKEN_BYTES)),
        };
        assert!(validate_http_config(&cfg)
            .unwrap_err()
            .contains("只允许回环地址"));
    }

    #[cfg(unix)]
    #[test]
    fn config_token_requires_private_unix_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("maild-api-perms-{suffix}.toml"));
        std::fs::write(&path, "token").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let cfg = ApiConfig {
            bearer_token: Some("a".repeat(MIN_BEARER_TOKEN_BYTES)),
            ..ApiConfig::default()
        };
        assert!(validate_config_file_security(&path, &cfg).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(validate_config_file_security(&path, &cfg).is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn api_config_is_service() {
        fn is_service<T: Service>() {}
        is_service::<ApiConfig>();
        is_service::<StoreConfig>();
        assert_eq!(ApiConfig::default().transport, "stdio");
        assert_eq!(ApiConfig::default().listen, DEFAULT_HTTP_LISTEN);
    }

    #[cfg(windows)]
    #[test]
    fn named_pipe_security_has_explicit_non_null_dacl() {
        use std::ptr::null_mut;
        use windows_sys::Win32::Security::{GetSecurityDescriptorDacl, ACL, PSECURITY_DESCRIPTOR};

        let security = NamedPipeSecurity::for_current_user().expect("current-user pipe ACL");
        assert!(!security.descriptor.is_null());
        let mut present = 0;
        let mut dacl: *mut ACL = null_mut();
        let mut defaulted = 0;
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(
                    security.descriptor as PSECURITY_DESCRIPTOR,
                    &mut present,
                    &mut dacl,
                    &mut defaulted,
                )
            },
            0
        );
        assert_eq!(present, 1, "explicit DACL must be present");
        assert!(!dacl.is_null(), "DACL must not be NULL");
    }
}
