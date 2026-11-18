//! Shared JSON-RPC client for Featherpost frontends.
//!
//! The daemon owns all mail-domain behavior. This crate only owns endpoint
//! discovery, local transport security checks, JSON-RPC framing and response
//! validation so CLI and GUI consumers do not grow separate protocol stacks.

use std::fmt;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf,
    WriteHalf,
};

mod plugin_package;

pub use plugin_package::{
    inspect_plugin_package, stage_plugin_package, stage_plugin_package_into, PluginPackageKind, PluginPackageSummary,
    StagedPluginPackage,
};

pub use maild_brand::{
    PLUGIN_TOKEN_ENV, SOCKET_ENV as API_SOCKET_ENV, TOKEN_ENV as API_TOKEN_ENV,
    URL_ENV as API_URL_ENV,
};
/// Windows named-pipe override: `<ENV_PREFIX>_PIPE`, the pipe sibling of [`API_SOCKET_ENV`].
pub const API_PIPE_ENV: &str = brand_env::PIPE;

mod brand_env {
    const SUFFIX: &str = "_PIPE";
    const LEN: usize = maild_brand::ENV_PREFIX.len() + SUFFIX.len();
    const BYTES: [u8; LEN] = {
        let mut out = [0u8; LEN];
        let (prefix, suffix) = (maild_brand::ENV_PREFIX.as_bytes(), SUFFIX.as_bytes());
        let mut i = 0;
        while i < prefix.len() {
            out[i] = prefix[i];
            i += 1;
        }
        while i < LEN {
            out[i] = suffix[i - prefix.len()];
            i += 1;
        }
        out
    };
    pub(crate) const PIPE: &str = match std::str::from_utf8(&BYTES) {
        Ok(value) => value,
        Err(_) => panic!("ENV_PREFIX must be UTF-8"),
    };
}
/// Pre-09-C egui shell. Never a catalog entry: persisted defaults naming it read as
/// [`DEFAULT_FRONTEND`], explicit requests route to [`BUILTIN_RECOVERY_FRONTEND`] (ADR-0039).
pub const LEGACY_EGUI_FRONTEND: &str = "builtin-egui";
pub const BUILTIN_RECOVERY_FRONTEND: &str = "builtin-repair";
/// The bundled Tauri frontend, installed and managed like any other frontend package.
pub const DEFAULT_FRONTEND: &str = maild_brand::BUNDLED_FRONTEND_ID;
mod native_recovery;
pub use native_recovery::{
    claim_main_ui, native_child_command, open_native_recovery, running_main_ui, system_home,
    MainUiClaim, MainUiLock, NativeAppLayout, MAIN_UI_LOCK,
};

pub fn is_builtin_frontend(id: &str) -> bool {
    matches!(id, LEGACY_EGUI_FRONTEND | BUILTIN_RECOVERY_FRONTEND)
}
pub const DEFAULT_CONFIG: &str = maild_paths::CONFIG_FILE_NAME;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;
pub const MAX_MAILBOX_BYTES: usize = 320;
pub const MAX_SUBJECT_BYTES: usize = 998;
pub const MAX_BODY_BYTES: usize = 512 * 1024;
pub const MAX_SILENT_KEYWORDS: usize = 128;
pub const MAX_SILENT_KEYWORD_BYTES: usize = 256;
pub const MAX_QUIET_HOURS: usize = 32;
pub const MIN_TIMEZONE_OFFSET_MINUTES: i32 = -24 * 60;
pub const MAX_TIMEZONE_OFFSET_MINUTES: i32 = 24 * 60;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONSECUTIVE_EMPTY_FRAMES: usize = 32;

fn validate_staging_name(staging: &str) -> Result<(), ClientError> {
    if (6..=64).contains(&staging.len())
        && staging
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !staging.starts_with('-')
    {
        Ok(())
    } else {
        Err(ClientError::rpc("staging 名无效"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Connection,
    Rpc,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Rpc => "rpc",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientError {
    pub kind: ErrorKind,
    pub message: String,
    pub rpc_code: Option<i64>,
    /// 错误对象的 `data`。daemon 往这里放**可据以行动**的东西——`-32006` 的当前版本号、
    /// `-32007` 的字段与上限（ADR-0034）——丢掉它调用方就只能显示一句笼统的失败。
    pub rpc_data: Option<Value>,
}

impl ClientError {
    pub fn connection(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Connection,
            message: message.into(),
            rpc_code: None,
            rpc_data: None,
        }
    }

    pub fn rpc(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Rpc,
            message: message.into(),
            rpc_code: None,
            rpc_data: None,
        }
    }

    fn rpc_response(error: JsonRpcError) -> Self {
        Self {
            kind: ErrorKind::Rpc,
            message: error.message,
            rpc_code: Some(error.code),
            rpc_data: error.data,
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.rpc_code {
            Some(code) => write!(f, "{} ({code})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ClientError {}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

impl Notification {
    pub fn into_server_notification(self) -> Result<ServerNotification, ClientError> {
        match self.method.as_str() {
            "message/received" => serde_json::from_value(self.params)
                .map(|message| ServerNotification::MessageReceived(Box::new(message)))
                .map_err(|error| {
                    ClientError::rpc(format!("message/received 通知参数无效: {error}"))
                }),
            "account/state" => serde_json::from_value(self.params)
                .map(ServerNotification::AccountState)
                .map_err(|error| ClientError::rpc(format!("account/state 通知参数无效: {error}"))),
            "frontend/intent" => serde_json::from_value::<FrontendIntentParams>(self.params)
                .map(|params| ServerNotification::FrontendIntent {
                    frontend: params.frontend,
                })
                .map_err(|error| {
                    ClientError::rpc(format!("frontend/intent 通知参数无效: {error}"))
                }),
            // ADR-0046：后台即将停机（菜单栏「退出 <产品名>」）。参数恒为空对象，不校验其内容——
            // 帧只是信号，旧/新 daemon 多带字段也不应让前端错过关闭。
            "agent/stopping" => Ok(ServerNotification::AgentStopping),
            _ => Ok(ServerNotification::Unknown(self)),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct AccountState {
    pub account: String,
    pub state: String,
    #[serde(default)]
    pub detail: String,
}

/// 附件元数据（ADR-0025）。字节不过协议帧——附件内容走独立取回路径（M4.5-04）。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct MailAttachment {
    pub part_index: u32,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub mime_type: String,
    /// `Content-ID`（尖括号已剥离），供正文里的 `cid:` 引用。
    #[serde(default)]
    pub content_id: String,
    #[serde(default)]
    pub inline: bool,
    pub size: u64,
    /// 字节是否已落库（ADR-0032 决策 3）。`false` = 超限或历史缓存，`readAttachment` 会得到
    /// `-32004`；旧 daemon 缺省 `false`——不确定时不制造可点的下载入口。
    #[serde(default)]
    pub stored: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct MailMessage {
    pub account: String,
    pub uid: String,
    pub sender: String,
    /// 收件人（ADR-0022 地址层；旧 daemon 缺省空串）。
    #[serde(default)]
    pub recipient: String,
    pub subject: String,
    pub body: String,
    /// **本机第一次观测到这封信的时刻**，不是发信时间（ADR-0025）。
    pub received_at: u64,

    // —— ADR-0025 新增；旧 daemon 一律缺省。——
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub in_reply_to: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub references: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cc: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bcc: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reply_to: String,
    /// 发信人声称的时间（`Date` 头）；缺失或不可解析为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_at: Option<u64>,
    /// 列表排序键：可信的 `date_at`，否则 `received_at`。
    #[serde(default)]
    pub sort_at: u64,
    /// HTML 原文。只有 `getMessage` 返回它；列表与通知一律为空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body_html: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub snippet: String,
    #[serde(default)]
    pub has_attachments: bool,
    #[serde(default)]
    pub seen: bool,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default)]
    pub answered: bool,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MailAttachment>,

    // —— ADR-0026：一封邮件的身份是 (account, folder, uid)。——
    /// 所属文件夹的服务器线上原名。旧 daemon 缺省空串。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// inbox/sent/drafts/trash/archive/spam，无角色为空串。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder_role: String,

    // —— ADR-0029：线程聚合。旧 daemon 缺省 0。——
    /// 所属线程 id（账户内唯一）。0 = 该消息尚未进入索引回填。
    #[serde(default)]
    pub thread_id: i64,
}

/// Reverse-transport request/response DTOs shared by the host and providers.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct TransportProbeRequest {
    pub account: String,
    pub address: String,
    pub direction: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct TransportProbeResponse {
    pub ok: bool,
    pub stage: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct TransportReceiveRequest {
    pub account: String,
    pub address: String,
    pub since_uid: String,
    pub max: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct TransportReceiveMessage {
    pub uid: String,
    pub sender: String,
    #[serde(default)]
    pub recipient: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    pub received_at: u64,
    // —— ADR-0025 新增可选键；缺省保持既有行为，现有 provider 无需改动。——
    /// RFC 5322 `Date` 头原文。
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub cc: String,
    #[serde(default)]
    pub reply_to: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct TransportReceiveResponse {
    pub messages: Vec<TransportReceiveMessage>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct TransportSendRequest {
    pub account: String,
    pub account_address: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct MessageSummary {
    pub account: String,
    pub uid: String,
    pub sender: String,
    /// 收件人（ADR-0022 地址层；旧 daemon 缺省空串）。
    #[serde(default)]
    pub recipient: String,
    pub subject: String,
    pub received_at: u64,

    // —— ADR-0025 新增；旧 daemon 一律缺省。——
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_at: Option<u64>,
    #[serde(default)]
    pub sort_at: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub snippet: String,
    #[serde(default)]
    pub has_attachments: bool,
    #[serde(default)]
    pub seen: bool,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default)]
    pub answered: bool,
    #[serde(default)]
    pub deleted: bool,

    // —— ADR-0026 ——
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder_role: String,

    // —— ADR-0029：线程聚合。旧 daemon 缺省 0。——
    /// 所属线程 id（账户内唯一）。0 = 该消息尚未进入索引回填。
    #[serde(default)]
    pub thread_id: i64,
}

/// 一个文件夹（ADR-0026）。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct MailFolder {
    pub account: String,
    /// 服务器线上原名，`listMessageSummaries` 的 `folder` 参数用它。
    pub name: String,
    /// 解码后的展示名（modified UTF-7 已还原）。
    #[serde(default)]
    pub display_name: String,
    /// inbox/sent/drafts/trash/archive/spam，无角色为空串。未知值原样保留。
    #[serde(default)]
    pub role: String,
    /// 是否出现在服务器最近一次 `LIST` 里。false = 不再同步，但消息保留。
    #[serde(default)]
    pub present: bool,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub unseen: i64,
}

impl From<&MailMessage> for MessageSummary {
    fn from(message: &MailMessage) -> Self {
        Self {
            account: message.account.clone(),
            uid: message.uid.clone(),
            sender: message.sender.clone(),
            recipient: message.recipient.clone(),
            subject: message.subject.clone(),
            received_at: message.received_at,
            date_at: message.date_at,
            sort_at: message.sort_at,
            snippet: message.snippet.clone(),
            has_attachments: message.has_attachments,
            seen: message.seen,
            flagged: message.flagged,
            answered: message.answered,
            deleted: message.deleted,
            folder: message.folder.clone(),
            folder_role: message.folder_role.clone(),
            thread_id: message.thread_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServerNotification {
    /// 装箱：ADR-0025 之后 `MailMessage` 远大于其他变体，不装箱会让通知队列里的
    /// 每一项都按最大变体分配。
    MessageReceived(Box<MailMessage>),
    AccountState(AccountState),
    /// ADR-0045：本连接主体（`frontend:<frontend>`）有新的待领导航意图。只是唤醒信号，
    /// 意图本身仍须 `claimFrontendIntent` 领取。
    FrontendIntent {
        frontend: String,
    },
    /// ADR-0046：daemon 即将停机（用户在菜单栏点了「退出 <产品名>」）。前端收到后应当刷盘草稿、
    /// 撤下窗口并随后台一起退出。
    AgentStopping,
    Unknown(Notification),
}

#[derive(Deserialize)]
struct FrontendIntentParams {
    frontend: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct StatusResult {
    pub version: String,
    #[serde(default)]
    pub accounts: Vec<AccountState>,
    pub message_count: i64,
    /// 常驻进程与本地库的资源占用（ADR-0041 决策 6）。旧 daemon 不回传 → `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeMetrics>,
}

/// `status.runtime`：平台拿不到的口径为 `None`（如 Linux 没有 `phys_footprint`）。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct RuntimeMetrics {
    #[serde(default)]
    pub rss_bytes: Option<u64>,
    #[serde(default)]
    pub phys_footprint_bytes: Option<u64>,
    #[serde(default)]
    pub store_db_bytes: Option<u64>,
    #[serde(default)]
    pub store_wal_bytes: Option<u64>,
    #[serde(default)]
    pub stored_messages: Option<u64>,
    #[serde(default)]
    pub stored_attachment_bytes: Option<u64>,
}

/// `syncMessages` 结果：被敲醒的账户名与其中此刻在线的个数（ADR-0041 决策 4）。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct SyncMessagesResult {
    #[serde(default)]
    pub triggered: Vec<String>,
    #[serde(default)]
    pub online_count: u64,
}

/// 自动发现出的一个服务器端点（ADR-0042）。`security` 取 `implicit`/`starttls`/… 原文。
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct DiscoveredEndpoint {
    pub server: String,
    pub port: u16,
    pub security: String,
}

/// `discoverEmailProvider` 结果（ADR-0042）。枚举型字段保留原文字符串，未来取值不破坏解析。
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ProviderDiscovery {
    pub provider_id: String,
    pub display_name: String,
    /// `preset` | `srv` | `tcp_probe` | `heuristic`
    pub source: String,
    /// `verified` | `high` | `guessed`
    pub confidence: String,
    pub incoming: DiscoveredEndpoint,
    #[serde(default)]
    pub mailbox: String,
    #[serde(default)]
    pub idle: bool,
    pub outgoing: DiscoveredEndpoint,
    #[serde(default)]
    pub username_format: String,
    #[serde(default)]
    pub suggested_username: String,
    #[serde(default)]
    pub auth_kind: String,
    #[serde(default)]
    pub strip_secret_spaces: bool,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub help_url: Option<String>,
}

/// `listEmailPresets` / `discoverEmailProvider({input: ""})` 结果。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct EmailPresetsResult {
    #[serde(default)]
    pub presets: Vec<ProviderDiscovery>,
}

/// 协议代数（ADR-0016）：服务端应答与客户端要求的同一常量单源于本 crate。
/// 不兼容契约变更（删方法、改 schema、改错误语义、收紧门禁）必须 +1；
/// 新增方法或可选字段不晋升。晋升规则见 docs/protocol/README.md。
pub const PROTOCOL_VERSION: u64 = 1;

const JSON_RPC_METHOD_NOT_FOUND: i64 = -32601;

/// `getAgentInfo` 应答。判定只依据 `protocol_version`；其余字段仅诊断展示，
/// 客户端不得让其参与逻辑（ADR-0016）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct AgentInfo {
    pub version: String,
    pub protocol_version: u64,
    pub pid: u32,
    #[serde(default)]
    pub started_at: Option<u64>,
    #[serde(default)]
    pub build_rev: Option<String>,
    #[serde(default)]
    pub executable_path: Option<String>,
}

/// `whoami` 应答（ADR-0017）：`user` / `plugin:<id>` / `frontend:<id>`。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct WhoamiResult {
    pub principal: String,
}

/// 握手判定结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatVerdict {
    /// 同一协议代，正常放行。
    Compatible,
    /// 对端内核较旧（含早于握手的遗留内核，-32601 视为代数 0）→ 引导重启内核。
    KernelOutdated,
    /// 当前应用较旧 → 引导更新应用。
    AppOutdated,
}

/// ADR-0016 判定表：相等兼容；对端小则内核过旧；对端大则应用过旧。
pub fn compat_verdict(required: u64, info: &AgentInfo) -> CompatVerdict {
    if info.protocol_version == required {
        CompatVerdict::Compatible
    } else if info.protocol_version < required {
        CompatVerdict::KernelOutdated
    } else {
        CompatVerdict::AppOutdated
    }
}

/// 握手应答 → 判定的共享规则，供 [`RpcClient`] 与连接 actor 句柄复用：
/// -32601 视为遗留内核（代数 0）；传输错误原样上抛，不得据此做版本结论。
pub fn compat_from_response(
    required: u64,
    response: Result<AgentInfo, ClientError>,
) -> Result<CompatVerdict, ClientError> {
    match response {
        Ok(info) => Ok(compat_verdict(required, &info)),
        Err(error) if error.rpc_code == Some(JSON_RPC_METHOD_NOT_FOUND) => {
            Ok(CompatVerdict::KernelOutdated)
        }
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationAvailability {
    Available,
    Unsupported,
    IdentityMismatch,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationAuthorization {
    Checking,
    NotDetermined,
    Denied,
    Authorized,
    Provisional,
    Ephemeral,
    NotApplicable,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct NotificationStatus {
    pub provider: String,
    pub availability: NotificationAvailability,
    pub authorization: NotificationAuthorization,
    pub request_in_flight: bool,
    pub fresh: bool,
}

/// One-shot navigation request produced by the headless core and claimed by a frontend.
///
/// The enum is intentionally independent from daemon internals so built-in and third-party
/// frontends can consume the same additive JSON-RPC contract.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum OpenIntent {
    Inbox {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    Message {
        account: String,
        uid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder: Option<String>,
    },
    Compose,
    Settings,
    Background,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct OpenIntentResult {
    pub intent: Option<OpenIntent>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct OpenFrontendResult {
    pub queued: bool,
    pub activation_requested: bool,
}

/// Public metadata for a discovered out-of-process frontend.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct FrontendInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub lifecycle: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// ADR-0030：纯授权声明（route 之外的部分）。旧 daemon 缺省空表。
    #[serde(default)]
    pub permissions: Vec<String>,
    pub builtin: bool,
    /// ADR-0017：注册表中是否有该包的身份条目（旧 daemon 缺省 false）。
    #[serde(default)]
    pub has_identity: bool,
    /// ADR-0018：本主体被拒绝的方法调用累计次数（内存态，重启清零）。
    #[serde(default)]
    pub denied_calls: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct FrontendsResult {
    pub default: String,
    #[serde(default)]
    pub frontends: Vec<FrontendInfo>,
}

/// Coarse lifecycle observation for an installed out-of-process plugin.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ExternalPluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub lifecycle: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub enabled: bool,
    pub state: String,
    pub restart_count: u32,
    /// ADR-0017：注册表中是否有该包的身份条目（旧 daemon 缺省 false）。
    #[serde(default)]
    pub has_identity: bool,
    /// ADR-0018：本主体被拒绝的方法调用累计次数（内存态，重启清零）。
    #[serde(default)]
    pub denied_calls: u64,
    /// ADR-0021：当前是否有活跃的 transport provider 注册（内存态）。
    #[serde(default)]
    pub provider_registered: bool,
    /// Whether the package declares a daemon-launchable native management UI.
    #[serde(default)]
    pub has_ui: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ExternalPluginsResult {
    #[serde(default)]
    pub plugins: Vec<ExternalPluginInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct OpenExternalPluginUiResult {
    pub id: String,
    pub launched: bool,
    pub already_running: bool,
}

/// Fully validated metadata used for package approval (ADR-0038).
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PackagePreview {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub lifecycle: String,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
    pub has_ui: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct CommitPluginInstallResult {
    pub action: String,
    pub id: String,
    #[serde(default)]
    pub from_version: Option<String>,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct SetPluginEnabledResult {
    pub id: String,
    pub enabled: bool,
}

/// 别名/一次性地址视图（M4-06，ADR-0022）。字段全容忍缺省：旧 daemon
/// 不会返回本类型；新 daemon 的后续演进字段对老 client 不可见即忽略。
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct AddressView {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub address: String,
    /// "alias" | "disposable"。
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    /// "active" | "disabled" | "expired"。
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct AddressesResult {
    #[serde(default)]
    pub addresses: Vec<AddressView>,
    #[serde(default)]
    pub total: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct AccountsResult {
    #[serde(default)]
    pub accounts: Vec<AccountState>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct MessagesResult {
    #[serde(default)]
    pub messages: Vec<MailMessage>,
    pub total: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct MessageSummariesResult {
    #[serde(default)]
    pub messages: Vec<MessageSummary>,
    pub total: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct FoldersResult {
    #[serde(default)]
    pub folders: Vec<MailFolder>,
}

/// 一次检索的结果（ADR-0029）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct SearchResult {
    #[serde(default)]
    pub messages: Vec<MessageSummary>,
    #[serde(default)]
    pub total: i64,
    /// `local` 或 `server`。
    #[serde(default)]
    pub scope: String,
    /// 仅服务器检索：命中数触到上限，还有更多没返回。
    #[serde(default)]
    pub truncated: bool,
}

/// 一条会话的全部消息，时间正序（ADR-0029 决策 7）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ThreadResult {
    /// 0 = 该消息尚未进入索引回填，`messages` 为空。
    #[serde(default, rename = "threadId")]
    pub thread_id: i64,
    #[serde(default)]
    pub messages: Vec<MessageSummary>,
}

/// 检索索引的回填进度（ADR-0029 决策 9）。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct SearchIndexStatus {
    /// 还没进索引的历史消息条数。回填期间检索结果是部分的。
    #[serde(default)]
    pub pending: i64,
    #[serde(default)]
    pub done: bool,
}

/// 一次本地检索的可选维度（ADR-0029 决策 4、5）。
///
/// `query` 是**纯文本**：daemon 自己切词并构造 FTS5 表达式，客户端不需要（也不应该）
/// 拼查询语法。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchRequest<'a> {
    pub query: &'a str,
    pub account: Option<&'a str>,
    pub folder: Option<&'a str>,
    /// 发件人子串，大小写不敏感。
    pub from: Option<&'a str>,
    /// 时间范围按列表显示的那个时间（`sort_at`）。
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub has_attachments: Option<bool>,
    pub limit: u64,
    pub offset: u64,
}

/// 写队列里的一条待办（ADR-0027）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct PendingOp {
    pub id: i64,
    pub account: String,
    pub folder: String,
    pub uid: String,
    /// `flag` 或 `move`。未知取值原样保留。
    pub kind: String,
    #[serde(default)]
    pub payload: String,
    /// `pending` / `in_flight` / `done` / `failed` / `unknown`。
    ///
    /// **`unknown` 不是 failed 的一种**：命令已发出但未见响应。对不可重放的操作
    /// （move）它是终态，必须由用户裁决。
    pub state: String,
    #[serde(default)]
    pub attempts: i64,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub detail: String,
    /// 需要用户裁决（重试或放弃）。
    #[serde(default)]
    pub needs_decision: bool,
}

/// Outgoing attachment reference: legacy message part, or a saved draft upload (ADR-0035).
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct DraftAttachment {
    #[serde(default)]
    pub draft_id: Option<String>,
    #[serde(default)]
    pub upload_id: Option<String>,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub uid: String,
    #[serde(default)]
    pub part_index: u32,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub size: u64,
}

impl Serialize for DraftAttachment {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match (&self.draft_id, &self.upload_id) {
            (Some(draft_id), Some(upload_id)) => {
                serde_json::json!({"draft_id": draft_id, "upload_id": upload_id}).serialize(serializer)
            }
            (None, None) => {
                // Root MIME parts use index zero. Older protocol-v1 daemons require this key.
                serde_json::json!({"account": self.account, "folder": self.folder, "uid": self.uid,
                    "part_index": self.part_index, "filename": self.filename, "size": self.size}).serialize(serializer)
            }
            _ => Err(serde::ser::Error::custom("draft_id and upload_id must appear together")),
        }
    }
}

/// `prepareReply` 返回的草稿。前端只负责编辑与提交。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct MessageDraft {
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub in_reply_to: String,
    #[serde(default)]
    pub references: String,
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct PrepareReplyResult {
    pub draft: MessageDraft,
}

/// 富消息发信的请求体（ADR-0028）。`bcc` **只进信封**，不会出现在报文头里。
#[derive(Clone, Debug, Default, Serialize, Eq, PartialEq)]
pub struct ComposeRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_address: Option<String>,
    pub to: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body_html: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub in_reply_to: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub references: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<DraftAttachment>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ComposeResult {
    #[serde(default)]
    pub ok: bool,
    /// 我们为这封信生成的 `Message-ID`（`send` 返回）。
    #[serde(default)]
    pub message_id: String,
    /// 是否已入队归档到已发送（`sendFromAccount` 返回）。
    #[serde(default)]
    pub archived: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct PendingOpsResult {
    #[serde(default)]
    pub ops: Vec<PendingOp>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct PendingOpResult {
    pub op: PendingOp,
}

/// 写入请求的应答：`queued` 是实际入队的条数（本地已不存在的 uid 会被跳过）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct WriteQueuedResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub queued: usize,
    /// `move` 或 `flag`——删除走了哪条路径（ADR-0027 决策 4）。
    #[serde(default)]
    pub mode: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct MessageResult {
    pub message: MailMessage,
    /// 此刻对这封信远程内容的策略结论（ADR-0032 决策 5）。旧 daemon 缺省 `None`。
    #[serde(default)]
    pub remote_content: Option<RemoteContentDecision>,
}

// —— ADR-0032：附件字节通道与阅读策略 ——

/// `readAttachment` 单块原始字节上限（与 daemon 侧常量一致）。
pub const MAX_ATTACHMENT_CHUNK_BYTES: u64 = 512 * 1024;

/// `getMessage.remote_content`：`decision` 取 `block` / `allow`；`reason` 取 `default` /
/// `trusted_address` / `trusted_domain` / `allow_all`。以字符串承载，未来新增值不破坏反序列化。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct RemoteContentDecision {
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub reason: String,
}

impl RemoteContentDecision {
    pub fn allows(&self) -> bool {
        self.decision == "allow"
    }
}

/// `readAttachment` 一块应答。`data` 是 base64；`length` 是本块原始字节数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct AttachmentChunk {
    pub attachment: MailAttachment,
    pub offset: u64,
    pub length: u64,
    pub data: String,
    pub eof: bool,
}

/// [`RpcClient::read_attachment`] 拼装完成的附件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentBytes {
    pub attachment: MailAttachment,
    pub bytes: Vec<u8>,
}

/// 一条可信发件人。`kind` 取 `address` / `domain`。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct TrustedSender {
    pub value: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub added_at: u64,
}

/// 阅读策略快照。`remote_content` 取 `block` / `allow`。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct ReadingPolicySnapshot {
    #[serde(default)]
    pub remote_content: String,
    #[serde(default)]
    pub trusted_senders: Vec<TrustedSender>,
}

/// `getReadingPolicy` / `setReadingPolicy` 应答。`persisted = false` 表示策略文件停摆：
/// 读到的是缺省值，写会被拒绝。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ReadingPolicyResult {
    pub policy: ReadingPolicySnapshot,
    #[serde(default)]
    pub persisted: bool,
}

/// 按已收字节推进 `offset` 循环取块，拼成完整附件。两道护栏：应答 `size` 中途改变即报错；
/// 累计字节超过首块宣称的 `size` 即报错——一个说谎的服务端不能让客户端无界分配。
/// `fetch_chunk(offset)` 由调用方提供：`RpcClient` 与 Tauri 壳（经连接 actor）共用这一个循环。
pub async fn assemble_attachment<F, Fut>(mut fetch_chunk: F) -> Result<AttachmentBytes, ClientError>
where
    F: FnMut(u64) -> Fut,
    Fut: std::future::Future<Output = Result<AttachmentChunk, ClientError>>,
{
    use base64::Engine as _;
    let mut bytes: Vec<u8> = Vec::new();
    let mut expected: Option<(MailAttachment, u64)> = None;
    loop {
        let chunk = fetch_chunk(bytes.len() as u64).await?;
        let size = chunk.attachment.size;
        match &expected {
            None => {
                if bytes.is_empty() {
                    bytes.reserve(usize::try_from(size).unwrap_or(0).min(64 * 1024 * 1024));
                }
                expected = Some((chunk.attachment.clone(), size));
            }
            Some((first, first_size)) => {
                if *first_size != size || first.part_index != chunk.attachment.part_index {
                    return Err(ClientError::rpc(
                        "readAttachment 应答的附件在读取途中发生变化".to_string(),
                    ));
                }
            }
        }
        if chunk.offset != bytes.len() as u64 {
            return Err(ClientError::rpc(format!(
                "readAttachment 应答 offset {} 与已收 {} 字节不符",
                chunk.offset,
                bytes.len()
            )));
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(chunk.data.as_bytes())
            .map_err(|error| ClientError::rpc(format!("readAttachment data 不是有效 base64: {error}")))?;
        if decoded.len() as u64 != chunk.length {
            return Err(ClientError::rpc(format!(
                "readAttachment 块长度 {} 与 length {} 不符",
                decoded.len(),
                chunk.length
            )));
        }
        if (bytes.len() as u64).saturating_add(decoded.len() as u64) > size {
            return Err(ClientError::rpc(format!(
                "readAttachment 累计字节超过宣称的 size {size}"
            )));
        }
        bytes.extend_from_slice(&decoded);
        if chunk.eof {
            if bytes.len() as u64 != size {
                return Err(ClientError::rpc(format!(
                    "readAttachment 在 {} 字节处 eof，宣称 size 为 {size}",
                    bytes.len()
                )));
            }
            let (attachment, _) = expected.expect("首块已记录");
            return Ok(AttachmentBytes { attachment, bytes });
        }
        if decoded.is_empty() {
            return Err(ClientError::rpc(
                "readAttachment 返回空块却未 eof".to_string(),
            ));
        }
    }
}

// —— 草稿（ADR-0034）——

/// 草稿的一条附件引用：指向库里已有的某封信的某个 part，字节不复制。
/// `stored` 是**读取时**现查的：源邮件可能在草稿写完之后被删掉。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct DraftAttachmentView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_id: Option<String>,
    pub account: String,
    pub folder: String,
    pub uid: String,
    pub part_index: u32,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub stored: bool,
}

/// ADR-0035 upload receipt; offset acknowledges exactly the received prefix.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct AttachmentUpload {
    pub upload_id: String,
    pub offset: u64,
    pub size: u64,
    pub chunk_bytes: usize,
}
#[derive(Deserialize)]
struct AttachmentUploadResult {
    upload: AttachmentUpload,
}

/// 一份完整草稿。`version` 是并发仲裁：写回时原样带上它。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct DraftView {
    #[serde(default)]
    pub send_state: String,
    #[serde(default)]
    pub sent_archive: Option<bool>,
    pub id: String,
    pub account: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub body_html: String,
    #[serde(default)]
    pub in_reply_to: String,
    #[serde(default)]
    pub references: String,
    #[serde(default)]
    pub attachments: Vec<DraftAttachmentView>,
    pub version: u64,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

/// 列表项：正文不进列表，只给单行摘要。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct DraftSummaryView {
    pub id: String,
    pub account: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub snippet: String,
    #[serde(default)]
    pub attachment_count: u32,
    pub version: u64,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct DraftListResult {
    #[serde(default)]
    pub drafts: Vec<DraftSummaryView>,
    #[serde(default)]
    pub total: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct DraftResult {
    pub draft: DraftView,
}

/// 写入侧的附件引用。`account` 缺省 = 草稿自己的账户；跨账户转发要显式写出来。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct DraftAttachmentInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default)]
    pub folder: String,
    #[serde(default)]
    pub uid: String,
    #[serde(default)]
    pub part_index: u32,
}

/// `createDraft` / `updateDraft` 的字段补丁：`None` = 不动，`Some` = 整体替换。
/// 清空正文是 `Some(String::new())`，不是 `None`。
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct DraftPatchInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cc: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bcc: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<DraftAttachmentInput>>,
}

/// `setReadingPolicy` 的原子 patch。空字段不发送；daemon 拒绝空 patch。
#[derive(Clone, Debug, Default, Serialize, Eq, PartialEq)]
pub struct ReadingPolicyPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trust: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub untrust: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct QuietHours {
    pub start: String,
    pub end: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct RulesSettings {
    pub show_sensitive: bool,
    #[serde(default)]
    pub silent_keywords: Vec<String>,
    #[serde(default)]
    pub quiet_hours: Vec<QuietHours>,
    pub timezone_offset_minutes: i32,
    /// 收件箱新邮件系统通知（ADR-0043）。旧 daemon 不回传 → `false`；为 `false` 时不序列化，
    /// 发给旧 daemon 的整包 patch 逐字节不变。
    #[serde(default, skip_serializing_if = "is_false")]
    pub notify_new_mail: bool,
    /// 不弹新邮件通知的账户（`MailMessage::account` 精确匹配）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub muted_accounts: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Default, Serialize, Eq, PartialEq)]
pub struct RulesPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_sensitive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silent_keywords: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<Vec<QuietHours>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone_offset_minutes: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_new_mail: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted_accounts: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct SettingsResult {
    pub rules: RulesSettings,
    #[serde(default)]
    pub persisted: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RulesField {
    SilentKeywords,
    SilentKeyword(usize),
    QuietHours,
    QuietHoursStart(usize),
    QuietHoursEnd(usize),
    TimezoneOffsetMinutes,
    MutedAccounts,
    MutedAccount(usize),
    Persisted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RulesValidationError {
    pub field: RulesField,
    pub message: String,
}

impl fmt::Display for RulesValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RulesValidationError {}

fn rules_error(field: RulesField, message: impl Into<String>) -> RulesValidationError {
    RulesValidationError {
        field,
        message: message.into(),
    }
}

fn validate_clock(field: RulesField, value: &str) -> Result<u16, RulesValidationError> {
    let invalid = |message: String| rules_error(field, message);
    let Some((hour, minute)) = value.split_once(':') else {
        return Err(invalid(format!("时间必须是 HH:MM: {value}")));
    };
    if hour.len() != 2 || minute.len() != 2 {
        return Err(invalid(format!("时间必须是两位 HH:MM: {value}")));
    }
    let hour: u16 = hour
        .parse()
        .map_err(|_| invalid(format!("小时无效: {value}")))?;
    let minute: u16 = minute
        .parse()
        .map_err(|_| invalid(format!("分钟无效: {value}")))?;
    if hour > 24 || minute >= 60 || (hour == 24 && minute != 0) {
        return Err(invalid(format!("时间超出范围: {value}")));
    }
    Ok((hour * 60 + minute) % (24 * 60))
}

fn validate_keywords(keywords: &[String]) -> Result<(), RulesValidationError> {
    if keywords.len() > MAX_SILENT_KEYWORDS {
        return Err(rules_error(
            RulesField::SilentKeywords,
            format!("静默关键词最多 {MAX_SILENT_KEYWORDS} 项"),
        ));
    }
    for (index, keyword) in keywords.iter().enumerate() {
        if keyword.len() > MAX_SILENT_KEYWORD_BYTES {
            return Err(rules_error(
                RulesField::SilentKeyword(index),
                format!("关键词最多 {MAX_SILENT_KEYWORD_BYTES} 字节"),
            ));
        }
    }
    Ok(())
}

fn validate_quiet_hours(windows: &[QuietHours]) -> Result<(), RulesValidationError> {
    if windows.len() > MAX_QUIET_HOURS {
        return Err(rules_error(
            RulesField::QuietHours,
            format!("静默时段最多 {MAX_QUIET_HOURS} 个"),
        ));
    }
    for (index, window) in windows.iter().enumerate() {
        let start = validate_clock(RulesField::QuietHoursStart(index), &window.start)?;
        let end = validate_clock(RulesField::QuietHoursEnd(index), &window.end)?;
        if start == end {
            return Err(rules_error(
                RulesField::QuietHoursEnd(index),
                format!(
                    "静默时段开始和结束不能相同: {}-{}",
                    window.start, window.end
                ),
            ));
        }
    }
    Ok(())
}

fn validate_timezone_offset(value: i32) -> Result<(), RulesValidationError> {
    if !(MIN_TIMEZONE_OFFSET_MINUTES..=MAX_TIMEZONE_OFFSET_MINUTES).contains(&value) {
        return Err(rules_error(
            RulesField::TimezoneOffsetMinutes,
            format!(
                "时区偏移必须在 {MIN_TIMEZONE_OFFSET_MINUTES}..={MAX_TIMEZONE_OFFSET_MINUTES} 分钟内"
            ),
        ));
    }
    Ok(())
}

/// 与 daemon `rules::MAX_MUTED_ACCOUNTS` / `MAX_MUTED_ACCOUNT_BYTES` 一致（ADR-0043）。
pub const MAX_MUTED_ACCOUNTS: usize = 64;
pub const MAX_MUTED_ACCOUNT_BYTES: usize = 128;

fn validate_muted_accounts(accounts: &[String]) -> Result<(), RulesValidationError> {
    if accounts.len() > MAX_MUTED_ACCOUNTS {
        return Err(rules_error(
            RulesField::MutedAccounts,
            format!("静音账户最多 {MAX_MUTED_ACCOUNTS} 项"),
        ));
    }
    for (index, account) in accounts.iter().enumerate() {
        if account.trim().is_empty()
            || account.len() > MAX_MUTED_ACCOUNT_BYTES
            || account.chars().any(char::is_control)
        {
            return Err(rules_error(
                RulesField::MutedAccount(index),
                format!("静音账户须非空、不含控制字符且不超过 {MAX_MUTED_ACCOUNT_BYTES} 字节"),
            ));
        }
    }
    Ok(())
}

pub fn validate_rules_settings(settings: &RulesSettings) -> Result<(), RulesValidationError> {
    validate_keywords(&settings.silent_keywords)?;
    validate_quiet_hours(&settings.quiet_hours)?;
    validate_muted_accounts(&settings.muted_accounts)?;
    validate_timezone_offset(settings.timezone_offset_minutes)
}

pub fn validate_rules_patch(patch: &RulesPatch) -> Result<(), RulesValidationError> {
    if let Some(keywords) = &patch.silent_keywords {
        validate_keywords(keywords)?;
    }
    if let Some(windows) = &patch.quiet_hours {
        validate_quiet_hours(windows)?;
    }
    if let Some(offset) = patch.timezone_offset_minutes {
        validate_timezone_offset(offset)?;
    }
    if let Some(accounts) = &patch.muted_accounts {
        validate_muted_accounts(accounts)?;
    }
    Ok(())
}

/// `require_persisted_flag`：`setSettings` 响应必须明确给出 `persisted`。ADR-0043 之后它
/// 可能是 `true`（`rules_state.toml` 已原子落盘），也可能是 `false`（只在内存）——调用方
/// 据此如实告诉用户，但缺这个键说明对端不是认识这条契约的 daemon。
pub fn validate_settings_result(
    result: &SettingsResult,
    require_persisted_flag: bool,
) -> Result<(), RulesValidationError> {
    validate_rules_settings(&result.rules)?;
    if require_persisted_flag && result.persisted.is_none() {
        return Err(rules_error(
            RulesField::Persisted,
            "setSettings 响应必须明确给出 persisted",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct SendRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

/// 受管账户发信请求。与 legacy `SendRequest` 分开：From 由账户或其 active
/// 别名决定，调用方不能提交任意发件地址。
#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct SendFromAccountRequest {
    pub account: String,
    pub to: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_address: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SendField {
    From,
    To,
    Subject,
    Body,
}

impl SendField {
    pub fn label(self) -> &'static str {
        match self {
            Self::From => "发件地址",
            Self::To => "收件地址",
            Self::Subject => "主题",
            Self::Body => "正文",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendValidationError {
    pub field: SendField,
    pub message: String,
}

impl fmt::Display for SendValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SendValidationError {}

pub fn validate_send_field(field: SendField, value: &str) -> Result<(), SendValidationError> {
    let invalid = |message: String| SendValidationError { field, message };
    match field {
        SendField::From | SendField::To => {
            if value.is_empty() {
                return Err(invalid(format!("{}必填", field.label())));
            }
            if value.len() > MAX_MAILBOX_BYTES {
                return Err(invalid(format!(
                    "{}过长，上限 {MAX_MAILBOX_BYTES} 字节",
                    field.label()
                )));
            }
            let Some((local, domain)) = value.split_once('@') else {
                return Err(invalid(format!("{}不是有效邮箱地址", field.label())));
            };
            let local_valid = !local.is_empty()
                && !local.starts_with('.')
                && !local.ends_with('.')
                && !local.contains("..")
                && local.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(
                            byte,
                            b'!' | b'#'
                                | b'$'
                                | b'%'
                                | b'&'
                                | b'\''
                                | b'*'
                                | b'+'
                                | b'-'
                                | b'/'
                                | b'='
                                | b'?'
                                | b'^'
                                | b'_'
                                | b'`'
                                | b'{'
                                | b'|'
                                | b'}'
                                | b'~'
                                | b'.'
                        )
                });
            let domain_valid = !domain.is_empty()
                && domain.len() <= 253
                && domain.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                });
            if !local_valid || !domain_valid {
                return Err(invalid(format!("{}不是有效邮箱地址", field.label())));
            }
        }
        SendField::Subject => {
            if value.len() > MAX_SUBJECT_BYTES {
                return Err(invalid(format!("主题过长，上限 {MAX_SUBJECT_BYTES} 字节")));
            }
            if value.chars().any(char::is_control) {
                return Err(invalid("主题不能包含控制字符".into()));
            }
        }
        SendField::Body => {
            if value.len() > MAX_BODY_BYTES {
                return Err(invalid(format!("正文过长，上限 {MAX_BODY_BYTES} 字节")));
            }
            if value.contains('\0') {
                return Err(invalid("正文不能包含 NUL".into()));
            }
        }
    }
    Ok(())
}

pub fn validate_send_request(message: &SendRequest) -> Result<(), SendValidationError> {
    for (field, value) in [
        (SendField::From, message.from.as_str()),
        (SendField::To, message.to.as_str()),
        (SendField::Subject, message.subject.as_str()),
        (SendField::Body, message.body.as_str()),
    ] {
        validate_send_field(field, value)?;
    }
    Ok(())
}

/// 连接上的入站帧。`Request` 是 ADR-0021 的反向调用：守护进程向已注册
/// provider 连接发起的 JSON-RPC 请求（带非空 id），插件必须以匹配 id 应答。
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Response {
        id: Value,
        result: Result<Value, JsonRpcError>,
    },
    Notification(Notification),
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

#[derive(Clone, Debug, Default)]
pub struct DiscoveryOptions {
    pub config_path: Option<PathBuf>,
    pub url: Option<String>,
    pub socket: Option<PathBuf>,
    pub pipe: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct EndpointEnvironment {
    url: Option<String>,
    socket: Option<PathBuf>,
    pipe: Option<String>,
    token: Option<String>,
}

#[derive(Clone, Deserialize)]
struct ApiEndpointConfig {
    #[serde(default = "default_transport")]
    transport: String,
    #[serde(default = "default_listen")]
    listen: String,
    #[serde(default)]
    bearer_token: Option<String>,
}

fn default_transport() -> String {
    "stdio".into()
}

fn default_listen() -> String {
    "127.0.0.1:7394".into()
}

#[derive(Clone)]
pub enum Endpoint {
    Http { url: String, token: String },
    Unix { path: PathBuf },
    NamedPipe { name: String },
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { url, .. } => f
                .debug_struct("Http")
                .field("url", url)
                .field("token", &"[redacted]")
                .finish(),
            Self::Unix { path } => f.debug_struct("Unix").field("path", path).finish(),
            Self::NamedPipe { name } => f.debug_struct("NamedPipe").field("name", name).finish(),
        }
    }
}

impl Endpoint {
    pub fn supports_notifications(&self) -> bool {
        !matches!(self, Self::Http { .. })
    }
}

pub fn discover_endpoint(options: &DiscoveryOptions) -> Result<Endpoint, ClientError> {
    let environment = EndpointEnvironment {
        url: std::env::var(API_URL_ENV).ok(),
        socket: std::env::var(API_SOCKET_ENV).ok().map(PathBuf::from),
        pipe: std::env::var(API_PIPE_ENV).ok(),
        token: std::env::var(API_TOKEN_ENV)
            .ok()
            .filter(|value| !value.is_empty()),
    };
    discover_endpoint_with_environment(options, environment)
}

fn discover_endpoint_with_environment(
    options: &DiscoveryOptions,
    environment: EndpointEnvironment,
) -> Result<Endpoint, ClientError> {
    let explicit_count = usize::from(options.url.is_some())
        + usize::from(options.socket.is_some())
        + usize::from(options.pipe.is_some());
    if explicit_count > 1 {
        return Err(ClientError::connection(
            "--url、--socket、--pipe 最多只能设置一个",
        ));
    }

    let (url, socket, pipe) = if explicit_count == 1 {
        (
            options.url.clone(),
            options.socket.clone(),
            options.pipe.clone(),
        )
    } else {
        let environment_count = usize::from(environment.url.is_some())
            + usize::from(environment.socket.is_some())
            + usize::from(environment.pipe.is_some());
        if environment_count > 1 {
            return Err(ClientError::connection(format!(
                "{API_URL_ENV}、{API_SOCKET_ENV}、{API_PIPE_ENV} 最多只能设置一个"
            )));
        }
        (environment.url, environment.socket, environment.pipe)
    };

    let has_endpoint_override = url.is_some() || socket.is_some() || pipe.is_some();
    let needs_http_token = url.is_some() && environment.token.is_none();
    let needs_config = !has_endpoint_override || needs_http_token;
    let should_read_config = needs_config || (url.is_some() && options.config_path.is_some());
    let config = if should_read_config {
        let resolved = maild_paths::resolve_config(options.config_path.as_deref())
            .map_err(|error| ClientError::connection(error.to_string()))?;
        let path = resolved.path;
        let text = match read_config_file(&path) {
            Ok(text) => text,
            Err(error)
                if options.config_path.is_none()
                    && !needs_config
                    && error.kind() == std::io::ErrorKind::NotFound =>
            {
                String::new()
            }
            Err(error) => {
                return Err(ClientError::connection(format!(
                    "读取配置失败 {}: {error}",
                    path.display()
                )))
            }
        };
        if text.is_empty() {
            None
        } else {
            let config = api_config_from_toml(&text)?
                .ok_or_else(|| ClientError::connection("配置中未启用 [plugins.api]"))?;
            Some((path, config))
        }
    } else {
        None
    };

    if let Some(url) = url {
        return http_endpoint(normalize_url(&url)?, environment.token, config.as_ref());
    }
    if let Some(path) = socket {
        return unix_endpoint(path);
    }
    if let Some(name) = pipe {
        return named_pipe_endpoint(name);
    }

    let config_entry = config
        .as_ref()
        .ok_or_else(|| ClientError::connection("无法从配置发现 API endpoint"))?;
    let config = &config_entry.1;
    match config.transport.as_str() {
        "http" => http_endpoint(
            normalize_url(&format!("http://{}", config.listen))?,
            environment.token,
            Some(config_entry),
        ),
        "unix" | "unix_socket" | "socket" => unix_endpoint(configured_unix_path(&config.listen)?),
        "named_pipe" | "pipe" => named_pipe_endpoint(config.listen.clone()),
        "ipc" => {
            #[cfg(unix)]
            {
                unix_endpoint(configured_unix_path(&config.listen)?)
            }
            #[cfg(windows)]
            {
                named_pipe_endpoint(config.listen.clone())
            }
            #[cfg(all(not(unix), not(windows)))]
            {
                Err(ClientError::connection("当前平台不支持 api IPC 传输"))
            }
        }
        "stdio" => Err(ClientError::connection(
            "独立客户端不能连接 stdio 传输；请配置 http、unix 或 named_pipe",
        )),
        other => Err(ClientError::connection(format!(
            "不支持的 api transport: {other}"
        ))),
    }
}

fn configured_unix_path(value: &str) -> Result<PathBuf, ClientError> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let paths = maild_paths::AppPaths::discover()
        .map_err(|error| ClientError::connection(error.to_string()))?;
    paths
        .runtime_path(path)
        .map_err(|error| ClientError::connection(error.to_string()))
}

fn read_config_file(path: &Path) -> io::Result<String> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "配置路径必须是普通文件",
        ));
    }
    if metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("配置文件超过 {MAX_CONFIG_BYTES} bytes"),
        ));
    }
    let capacity = usize::try_from(metadata.len())
        .unwrap_or(MAX_CONFIG_BYTES)
        .min(MAX_CONFIG_BYTES);
    let mut bytes = Vec::with_capacity(capacity);
    File::open(path)?
        .take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("配置文件超过 {MAX_CONFIG_BYTES} bytes"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "配置文件必须是 UTF-8"))
}

fn api_config_from_toml(text: &str) -> Result<Option<ApiEndpointConfig>, ClientError> {
    let root: toml::Value = toml::from_str(text)
        .map_err(|error| ClientError::connection(format!("TOML 解析失败: {}", error.message())))?;
    let Some(plugins) = root.get("plugins") else {
        return Ok(None);
    };
    let plugins = plugins
        .as_table()
        .ok_or_else(|| ClientError::connection("配置结构错误: `plugins` 必须是表"))?;
    let Some(entry) = plugins.get("api") else {
        return Ok(None);
    };
    if entry.get("enabled").and_then(toml::Value::as_bool) == Some(false) {
        return Ok(None);
    }
    let config = entry
        .get("config")
        .cloned()
        .unwrap_or_else(|| toml::Value::Table(toml::Table::new()));
    config
        .try_into()
        .map(Some)
        .map_err(|error: toml::de::Error| {
            ClientError::connection(format!("插件 api 配置错误: {}", error.message()))
        })
}

fn http_endpoint(
    url: String,
    environment_token: Option<String>,
    config: Option<&(PathBuf, ApiEndpointConfig)>,
) -> Result<Endpoint, ClientError> {
    let token = match environment_token {
        Some(token) => token,
        None => {
            let (path, config) = config.ok_or_else(missing_token_error)?;
            validate_config_file_security(path, config.bearer_token.is_some())?;
            config
                .bearer_token
                .clone()
                .ok_or_else(missing_token_error)?
        }
    };
    Ok(Endpoint::Http { url, token })
}

fn missing_token_error() -> ClientError {
    ClientError::connection(format!(
        "缺少 bearer token：设置 {API_TOKEN_ENV} 或配置 plugins.api.config.bearer_token"
    ))
}

fn normalize_url(url: &str) -> Result<String, ClientError> {
    let url = if url.ends_with("/rpc") {
        url.to_owned()
    } else {
        format!("{}/rpc", url.trim_end_matches('/'))
    };
    validate_http_url(&url)?;
    Ok(url)
}

fn validate_http_url(url: &str) -> Result<(), ClientError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| ClientError::connection(format!("无效 API URL: {error}")))?;
    if parsed.scheme() != "http" {
        return Err(ClientError::connection("API URL 只允许 http:// 回环地址"));
    }
    let host = parsed
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .ok_or_else(|| ClientError::connection("API URL host 必须是回环 IP 字面量"))?;
    if !host.is_loopback() {
        return Err(ClientError::connection("API URL 只允许回环地址"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ClientError::connection("API URL 不允许包含 userinfo"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ClientError::connection(
            "API URL 不允许包含 query 或 fragment",
        ));
    }
    Ok(())
}

fn unix_endpoint(path: PathBuf) -> Result<Endpoint, ClientError> {
    validate_unix_path(&path)?;
    Ok(Endpoint::Unix { path })
}

fn validate_unix_path(path: &Path) -> Result<(), ClientError> {
    #[cfg(unix)]
    {
        if !path.is_absolute() {
            return Err(ClientError::connection(format!(
                "Unix socket 路径必须是绝对路径: {}",
                path.display()
            )));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(ClientError::connection(
            "Unix socket 传输只在 Unix 平台可用",
        ))
    }
}

fn named_pipe_endpoint(name: String) -> Result<Endpoint, ClientError> {
    validate_named_pipe_name(&name)?;
    Ok(Endpoint::NamedPipe { name })
}

fn validate_named_pipe_name(name: &str) -> Result<(), ClientError> {
    #[cfg(windows)]
    {
        if !name.starts_with(r"\\.\pipe\") || name.len() == r"\\.\pipe\".len() {
            return Err(ClientError::connection(format!(
                r"named pipe 必须以 \\.\pipe\ 开头: {name}"
            )));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Err(ClientError::connection(
            "named pipe 传输只在 Windows 平台可用",
        ))
    }
}

fn validate_endpoint(endpoint: &Endpoint) -> Result<(), ClientError> {
    match endpoint {
        Endpoint::Http { url, .. } => validate_http_url(url),
        Endpoint::Unix { path } => validate_unix_path(path),
        Endpoint::NamedPipe { name } => validate_named_pipe_name(name),
    }
}

fn validate_config_file_security(path: &Path, has_token: bool) -> Result<(), ClientError> {
    if !has_token {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(path)
            .map_err(|error| {
                ClientError::connection(format!("读取配置权限失败 {}: {error}", path.display()))
            })?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(ClientError::connection(format!(
                "HTTP bearer token 所在配置文件权限过宽: {}（当前 {:o}，请 chmod 600）",
                path.display(),
                mode & 0o777
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn generate_token() -> Result<String, ClientError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| ClientError::connection(format!("生成随机 token 失败: {error}")))?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(token)
}

pub struct RpcClient {
    endpoint: Endpoint,
    http: Option<reqwest::Client>,
    next_id: AtomicU64,
}

impl RpcClient {
    pub fn new(endpoint: Endpoint) -> Result<Self, ClientError> {
        validate_endpoint(&endpoint)?;
        let http = if matches!(endpoint, Endpoint::Http { .. }) {
            Some(
                reqwest::Client::builder()
                    .timeout(REQUEST_TIMEOUT)
                    .redirect(reqwest::redirect::Policy::none())
                    .no_proxy()
                    .build()
                    .map_err(|error| {
                        ClientError::connection(format!("初始化 HTTP 客户端失败: {error}"))
                    })?,
            )
        } else {
            None
        };
        Ok(Self {
            endpoint,
            http,
            next_id: AtomicU64::new(1),
        })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = request(method, params, id)?;
        match &self.endpoint {
            Endpoint::Http { url, token } => {
                rpc_http(
                    self.http.as_ref().expect("HTTP endpoint has a client"),
                    url,
                    token,
                    &request,
                    &json!(id),
                )
                .await
            }
            Endpoint::Unix { .. } | Endpoint::NamedPipe { .. } => {
                let mut connection = connect_stream(&self.endpoint).await?;
                connection
                    .call_request(request, json!(id), REQUEST_TIMEOUT, |_| {})
                    .await
            }
        }
    }

    pub async fn status(&self) -> Result<StatusResult, ClientError> {
        self.call_typed("status", json!({})).await
    }

    /// 内核身份查询（ADR-0016）。不做判定；需要结论时用 [`Self::agent_compat`]。
    pub async fn agent_info(&self) -> Result<AgentInfo, ClientError> {
        self.call_typed("getAgentInfo", json!({})).await
    }

    /// 诊断：查询当前会话主体（恒为 `user`——[`Self::call`] 每请求独立连接，
    /// 插件主体只能经 [`StreamConnection::authenticate`] 绑定的长连接获得）。
    pub async fn whoami(&self) -> Result<String, ClientError> {
        let result: WhoamiResult = self.call_typed("whoami", json!({})).await?;
        Ok(result.principal)
    }

    /// reveal 即轮换：取回某已安装插件的明文凭证，旧凭证对新连接立即失效。
    /// 明文只在本次应答出现；服务端要求同用户 IPC 来源。
    pub async fn reveal_plugin_token(
        &self,
        id: &str,
        kind: Option<&str>,
    ) -> Result<String, ClientError> {
        let result = self
            .call(
                "revealPluginToken",
                match kind {
                    Some(kind) => json!({"id": id, "kind": kind}),
                    None => json!({"id": id}),
                },
            )
            .await?;
        result["token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ClientError::rpc("revealPluginToken 应答缺少 token"))
    }

    /// 版本握手：成功按判定表映射；对端无此方法（-32601）视为遗留内核。
    /// 传输错误原样上抛——调用方按连接故障处理，不得据此做版本结论。
    pub async fn agent_compat(&self, required: u64) -> Result<CompatVerdict, ClientError> {
        compat_from_response(required, self.agent_info().await)
    }

    pub async fn notification_status(&self) -> Result<NotificationStatus, ClientError> {
        self.call_typed("getNotificationStatus", json!({})).await
    }

    pub async fn request_notification_authorization(
        &self,
    ) -> Result<NotificationStatus, ClientError> {
        self.call_typed("requestNotificationAuthorization", json!({}))
            .await
    }

    pub async fn claim_next_open_intent(
        &self,
        frontend: &str,
    ) -> Result<OpenIntentResult, ClientError> {
        self.call_typed("claimNextOpenIntent", json!({ "frontend": frontend }))
            .await
    }

    pub async fn open_frontend(
        &self,
        intent: &OpenIntent,
    ) -> Result<OpenFrontendResult, ClientError> {
        self.open_frontend_with(None, intent).await
    }

    /// Route through a specific installed frontend. `None` uses the daemon default.
    pub async fn open_frontend_with(
        &self,
        frontend: Option<&str>,
        intent: &OpenIntent,
    ) -> Result<OpenFrontendResult, ClientError> {
        let params = match frontend {
            Some(frontend) => json!({ "frontend": frontend, "intent": intent }),
            None => json!({ "intent": intent }),
        };
        self.call_typed("openFrontend", params).await
    }

    pub async fn list_frontends(&self) -> Result<FrontendsResult, ClientError> {
        self.call_typed("listFrontends", json!({})).await
    }

    pub async fn list_external_plugins(&self) -> Result<ExternalPluginsResult, ClientError> {
        self.call_typed("listExternalPlugins", json!({})).await
    }

    pub async fn open_external_plugin_ui(
        &self,
        id: &str,
    ) -> Result<OpenExternalPluginUiResult, ClientError> {
        validate_provider_id("id", id).map_err(|error| ClientError::rpc(error.to_string()))?;
        self.call_typed("openExternalPluginUi", json!({ "id": id }))
            .await
    }

    pub async fn commit_plugin_install(
        &self,
        staging: &str,
    ) -> Result<CommitPluginInstallResult, ClientError> {
        validate_staging_name(staging)?;
        self.call_typed("commitPluginInstall", json!({ "staging": staging }))
            .await
    }

    pub async fn set_plugin_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<SetPluginEnabledResult, ClientError> {
        validate_provider_id("id", id).map_err(|error| ClientError::rpc(error.to_string()))?;
        self.call_typed("setPluginEnabled", json!({ "id": id, "enabled": enabled }))
            .await
    }

    pub async fn accounts(&self) -> Result<AccountsResult, ClientError> {
        self.call_typed("listAccounts", json!({})).await
    }

    pub async fn messages(&self, limit: u64, offset: u64) -> Result<MessagesResult, ClientError> {
        self.call_typed("listMessages", json!({ "limit": limit, "offset": offset }))
            .await
    }

    pub async fn message_summaries(
        &self,
        account: Option<&str>,
        limit: u64,
        offset: u64,
    ) -> Result<MessageSummariesResult, ClientError> {
        self.message_summaries_in_folder(account, None, limit, offset)
            .await
    }

    /// `folder` 是服务器线上原名（见 [`MailFolder::name`]）；`None` = 不按文件夹过滤。
    pub async fn message_summaries_in_folder(
        &self,
        account: Option<&str>,
        folder: Option<&str>,
        limit: u64,
        offset: u64,
    ) -> Result<MessageSummariesResult, ClientError> {
        self.call_typed(
            "listMessageSummaries",
            json!({
                "account": account,
                "folder": folder,
                "limit": limit,
                "offset": offset,
            }),
        )
        .await
    }

    /// `listFolders`：账户的文件夹清单（ADR-0026）。
    pub async fn folders(&self, account: Option<&str>) -> Result<FoldersResult, ClientError> {
        self.call_typed("listFolders", json!({ "account": account }))
            .await
    }

    /// `searchMessages`：本地全文检索 + 结构化过滤（ADR-0029）。
    pub async fn search(&self, request: &SearchRequest<'_>) -> Result<SearchResult, ClientError> {
        self.call_typed(
            "searchMessages",
            json!({
                "query": request.query,
                "account": request.account,
                "folder": request.folder,
                "from": request.from,
                "since": request.since,
                "until": request.until,
                "hasAttachments": request.has_attachments,
                "limit": request.limit.clamp(1, 200),
                "offset": request.offset,
            }),
        )
        .await
    }

    /// `searchMessages` 的服务器侧变体：走账户那条 IMAP 连接的 `SEARCH`，
    /// 用于本地索引覆盖不到的历史（ADR-0029 决策 11）。结果**不落库**。
    pub async fn search_on_server(
        &self,
        account: &str,
        folder: &str,
        query: &str,
    ) -> Result<SearchResult, ClientError> {
        self.call_typed(
            "searchMessages",
            json!({
                "account": account,
                "folder": folder,
                "query": query,
                "scope": "server",
            }),
        )
        .await
    }

    /// `getThread`：某封消息所在会话的全部消息，时间正序，跨文件夹。
    pub async fn thread(
        &self,
        account: &str,
        folder: Option<&str>,
        uid: &str,
    ) -> Result<ThreadResult, ClientError> {
        self.call_typed(
            "getThread",
            json!({ "account": account, "folder": folder, "uid": uid }),
        )
        .await
    }

    /// `getSearchIndexStatus`：索引回填进度。
    pub async fn search_index_status(&self) -> Result<SearchIndexStatus, ClientError> {
        self.call_typed("getSearchIndexStatus", json!({})).await
    }

    /// `markMessages`：置位或取消一个标记。本地立刻生效，写回排队（ADR-0027）。
    pub async fn mark_messages(
        &self,
        account: &str,
        folder: &str,
        uids: &[String],
        flag: &str,
        value: bool,
    ) -> Result<WriteQueuedResult, ClientError> {
        self.call_typed(
            "markMessages",
            json!({
                "account": account,
                "folder": folder,
                "uids": uids,
                "flag": flag,
                "value": value,
            }),
        )
        .await
    }

    /// `moveMessages`：移动到另一个文件夹（`target` 取 [`MailFolder::name`]）。
    pub async fn move_messages(
        &self,
        account: &str,
        folder: &str,
        uids: &[String],
        target: &str,
    ) -> Result<WriteQueuedResult, ClientError> {
        self.call_typed(
            "moveMessages",
            json!({
                "account": account,
                "folder": folder,
                "uids": uids,
                "target": target,
            }),
        )
        .await
    }

    /// `deleteMessages`：有废纸篓就移进去，否则落 `\Deleted`。
    pub async fn delete_messages(
        &self,
        account: &str,
        folder: &str,
        uids: &[String],
    ) -> Result<WriteQueuedResult, ClientError> {
        self.call_typed(
            "deleteMessages",
            json!({ "account": account, "folder": folder, "uids": uids }),
        )
        .await
    }

    /// `prepareReply`：在内核侧成型回复/全部回复/转发草稿（ADR-0028）。
    /// `mode` 取 `reply` / `reply_all` / `forward`。
    pub async fn prepare_reply(
        &self,
        account: &str,
        folder: Option<&str>,
        uid: &str,
        mode: &str,
    ) -> Result<PrepareReplyResult, ClientError> {
        self.call_typed(
            "prepareReply",
            json!({ "account": account, "folder": folder, "uid": uid, "mode": mode }),
        )
        .await
    }

    /// 富消息发信。`account` 非空时走 `sendFromAccount`（受管账户、可选别名 From），
    /// 否则走全局 `send`。
    pub async fn compose(&self, request: &ComposeRequest) -> Result<ComposeResult, ClientError> {
        let method = if request.account.is_some() {
            "sendFromAccount"
        } else {
            "send"
        };
        let params = serde_json::to_value(request)
            .map_err(|error| ClientError::rpc(format!("compose 请求无法序列化: {error}")))?;
        self.call_typed(method, params).await
    }

    /// `listPendingOps`：写队列观测。默认只返回未落定的。
    pub async fn pending_ops(
        &self,
        account: Option<&str>,
        include_settled: bool,
    ) -> Result<PendingOpsResult, ClientError> {
        self.call_typed(
            "listPendingOps",
            json!({ "account": account, "include_settled": include_settled }),
        )
        .await
    }

    /// `resolvePendingOp`：对「失败」或「结果未知」的裁决。
    pub async fn resolve_pending_op(
        &self,
        id: i64,
        retry: bool,
    ) -> Result<PendingOpResult, ClientError> {
        self.call_typed(
            "resolvePendingOp",
            json!({ "id": id, "resolution": if retry { "retry" } else { "discard" } }),
        )
        .await
    }

    pub async fn message(&self, account: &str, uid: &str) -> Result<MessageResult, ClientError> {
        self.call_typed("getMessage", json!({ "account": account, "uid": uid }))
            .await
    }

    /// 带 `folder` 的 `getMessage`（ADR-0026：UID 只在 mailbox 内唯一）。
    pub async fn message_in_folder(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
    ) -> Result<MessageResult, ClientError> {
        self.call_typed(
            "getMessage",
            json!({ "account": account, "folder": folder, "uid": uid }),
        )
        .await
    }

    /// `readAttachment` 单块（ADR-0032 决策 1）。`length` 为 `None` 时取 daemon 上限。
    pub async fn read_attachment_chunk(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
        part_index: u32,
        offset: u64,
        length: Option<u64>,
    ) -> Result<AttachmentChunk, ClientError> {
        let mut params = json!({
            "account": account,
            "folder": folder,
            "uid": uid,
            "part_index": part_index,
            "offset": offset,
        });
        if let Some(length) = length {
            params["length"] = json!(length);
        }
        self.call_typed("readAttachment", params).await
    }

    /// 循环读块直到 `eof`，拼成完整字节（护栏见 [`assemble_attachment`]）。
    pub async fn read_attachment(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
        part_index: u32,
    ) -> Result<AttachmentBytes, ClientError> {
        assemble_attachment(|offset| {
            self.read_attachment_chunk(account, folder, uid, part_index, offset, None)
        })
        .await
    }

    /// `listDrafts`（ADR-0034）。`account = None` 跨账户列出。
    pub async fn list_drafts(
        &self,
        account: Option<&str>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> Result<DraftListResult, ClientError> {
        let mut params = json!({});
        if let Some(account) = account {
            params["account"] = json!(account);
        }
        if let Some(limit) = limit {
            params["limit"] = json!(limit);
        }
        if let Some(offset) = offset {
            params["offset"] = json!(offset);
        }
        self.call_typed("listDrafts", params).await
    }

    pub async fn begin_attachment_upload(
        &self,
        draft_id: &str,
        filename: &str,
        mime_type: &str,
        size: u64,
    ) -> Result<AttachmentUpload, ClientError> {
        let r: AttachmentUploadResult = self.call_typed("beginAttachmentUpload", json!({"draft_id": draft_id, "filename": filename, "mime_type": mime_type, "size": size})).await?;
        Ok(r.upload)
    }
    pub async fn write_attachment_upload(
        &self,
        upload_id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<AttachmentUpload, ClientError> {
        use base64::Engine;
        if bytes.is_empty() || bytes.len() > 512 * 1024 {
            return Err(ClientError::rpc("上传块必须在 1..512 KiB 之间"));
        }
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| ClientError::rpc("上传偏移超出范围"))?;
        let r: AttachmentUploadResult = self.call_typed("writeAttachmentUpload", json!({"upload_id": upload_id, "offset": offset, "data": base64::engine::general_purpose::STANDARD.encode(bytes)})).await?;
        if r.upload.upload_id != upload_id
            || r.upload.offset < end
            || r.upload.offset > r.upload.size
        {
            return Err(ClientError::rpc("上传回执身份或偏移不匹配"));
        }
        Ok(r.upload)
    }
    pub async fn finish_attachment_upload(
        &self,
        upload_id: &str,
        base_version: u64,
    ) -> Result<DraftView, ClientError> {
        let r: DraftResult = self
            .call_typed(
                "finishAttachmentUpload",
                json!({"upload_id": upload_id, "base_version": base_version}),
            )
            .await?;
        Ok(r.draft)
    }
    pub async fn cancel_attachment_upload(&self, upload_id: &str) -> Result<(), ClientError> {
        self.call("cancelAttachmentUpload", json!({"upload_id": upload_id}))
            .await?;
        Ok(())
    }

    pub async fn get_draft(&self, id: &str) -> Result<DraftView, ClientError> {
        let result: DraftResult = self.call_typed("getDraft", json!({ "id": id })).await?;
        Ok(result.draft)
    }

    pub async fn create_draft(
        &self,
        account: &str,
        patch: &DraftPatchInput,
    ) -> Result<DraftView, ClientError> {
        let mut params = serde_json::to_value(patch)
            .map_err(|error| ClientError::rpc(format!("草稿 patch 序列化失败: {error}")))?;
        params["account"] = json!(account);
        let result: DraftResult = self.call_typed("createDraft", params).await?;
        Ok(result.draft)
    }

    /// `updateDraft`：`base_version` 与当前版本不符时 daemon 返回 `-32006` 且不写入任何内容。
    pub async fn update_draft(
        &self,
        id: &str,
        base_version: u64,
        patch: &DraftPatchInput,
    ) -> Result<DraftView, ClientError> {
        let mut params = serde_json::to_value(patch)
            .map_err(|error| ClientError::rpc(format!("草稿 patch 序列化失败: {error}")))?;
        params["id"] = json!(id);
        params["base_version"] = json!(base_version);
        let result: DraftResult = self.call_typed("updateDraft", params).await?;
        Ok(result.draft)
    }

    /// `deleteDraft`。`base_version` 为 `None` 时无条件丢弃。
    pub async fn delete_draft(
        &self,
        id: &str,
        base_version: Option<u64>,
    ) -> Result<DraftView, ClientError> {
        let mut params = json!({ "id": id });
        if let Some(version) = base_version {
            params["base_version"] = json!(version);
        }
        let result: DraftResult = self.call_typed("deleteDraft", params).await?;
        Ok(result.draft)
    }

    pub async fn reading_policy(&self) -> Result<ReadingPolicyResult, ClientError> {
        self.call_typed("getReadingPolicy", json!({})).await
    }

    pub async fn set_reading_policy(
        &self,
        patch: &ReadingPolicyPatch,
    ) -> Result<ReadingPolicyResult, ClientError> {
        let params = serde_json::to_value(patch)
            .map_err(|error| ClientError::rpc(format!("阅读策略 patch 序列化失败: {error}")))?;
        self.call_typed("setReadingPolicy", params).await
    }

    /// 敲醒后台立即同步（ADR-0041 决策 4）。`account = None` = 全部启用账户。不等同步完成。
    pub async fn sync_messages(
        &self,
        account: Option<&str>,
    ) -> Result<SyncMessagesResult, ClientError> {
        let params = match account {
            Some(account) => json!({ "account": account }),
            None => json!({}),
        };
        self.call_typed("syncMessages", params).await
    }

    /// 按 UID 取一封服务器上的邮件：本地已缓存直接返回，否则经账户连接拉回并落库
    /// （与 [`Self::search_on_server`] 配合补齐冷启动窗口之外的历史）。
    pub async fn fetch_server_message(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
    ) -> Result<MessageResult, ClientError> {
        self.call_typed(
            "fetchServerMessage",
            json!({ "account": account, "folder": folder, "uid": uid }),
        )
        .await
    }

    /// 服务商自动发现（ADR-0042）。`allow_network = false` 时零网络，只查内置知识库与启发式。
    pub async fn discover_email_provider(
        &self,
        input: &str,
        allow_network: bool,
    ) -> Result<ProviderDiscovery, ClientError> {
        if input.trim().is_empty() {
            return Err(ClientError::rpc("请输入邮箱地址或域名"));
        }
        self.call_typed(
            "discoverEmailProvider",
            json!({ "input": input, "allow_network": allow_network }),
        )
        .await
    }

    /// 常用服务商预设卡片（ADR-0042）。
    pub async fn list_email_presets(&self) -> Result<EmailPresetsResult, ClientError> {
        self.call_typed("listEmailPresets", json!({})).await
    }

    pub async fn settings(&self) -> Result<SettingsResult, ClientError> {
        let result: SettingsResult = self.call_typed("getSettings", json!({})).await?;
        validate_settings_result(&result, false)
            .map_err(|error| ClientError::rpc(format!("getSettings 响应无效: {error}")))?;
        Ok(result)
    }

    pub async fn set_settings(&self, patch: &RulesPatch) -> Result<SettingsResult, ClientError> {
        validate_rules_patch(patch)
            .map_err(|error| ClientError::rpc(format!("rules 设置无效: {error}")))?;
        let result: SettingsResult = self
            .call_typed("setSettings", json!({ "rules": patch }))
            .await?;
        validate_settings_result(&result, true)
            .map_err(|error| ClientError::rpc(format!("setSettings 响应无效: {error}")))?;
        Ok(result)
    }

    pub async fn send(&self, message: &SendRequest) -> Result<(), ClientError> {
        #[derive(Deserialize)]
        struct SendResult {
            ok: bool,
        }

        validate_send_request(message).map_err(|error| ClientError::rpc(error.to_string()))?;
        let result: SendResult = self.call_typed("send", json!(message)).await?;
        if !result.ok {
            return Err(ClientError::rpc("send 响应缺少 ok=true"));
        }
        Ok(())
    }

    async fn call_typed<T>(&self, method: &str, params: Value) -> Result<T, ClientError>
    where
        T: serde::de::DeserializeOwned,
    {
        let value = self.call(method, params).await?;
        serde_json::from_value(value)
            .map_err(|error| ClientError::rpc(format!("{method} 响应结构无效: {error}")))
    }

    pub async fn connect_stream(&self) -> Result<StreamConnection, ClientError> {
        connect_stream(&self.endpoint).await
    }
}

fn request(method: &str, params: Value, id: u64) -> Result<Value, ClientError> {
    if method.is_empty() {
        return Err(ClientError::rpc("JSON-RPC method 不能为空"));
    }
    let request = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": id,
    });
    let encoded = serde_json::to_vec(&request)
        .map_err(|error| ClientError::rpc(format!("序列化 JSON-RPC 请求失败: {error}")))?;
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(ClientError::rpc(format!(
            "JSON-RPC 请求超过 {MAX_FRAME_BYTES} bytes"
        )));
    }
    Ok(request)
}

async fn rpc_http(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    request: &Value,
    expected_id: &Value,
) -> Result<Value, ClientError> {
    let response = client
        .post(url)
        .bearer_auth(token)
        .json(request)
        .send()
        .await
        .map_err(|error| ClientError::connection(format!("连接 {url} 失败: {error}")))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ClientError::connection("HTTP 鉴权失败"));
    }
    if !response.status().is_success() {
        return Err(ClientError::connection(format!(
            "HTTP 请求失败: {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_FRAME_BYTES as u64)
    {
        return Err(ClientError::rpc(format!(
            "JSON-RPC 响应超过 {MAX_FRAME_BYTES} bytes"
        )));
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ClientError::rpc(format!("读取 JSON-RPC 响应失败: {error}")))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_FRAME_BYTES {
            return Err(ClientError::rpc(format!(
                "JSON-RPC 响应超过 {MAX_FRAME_BYTES} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    finish_response(decode_incoming(&bytes)?, expected_id)
}

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> AsyncStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}
type BoxedStream = Box<dyn AsyncStream>;

pub struct StreamConnection {
    reader: StreamReader,
    writer: StreamWriter,
}

pub struct StreamReader {
    reader: BufReader<ReadHalf<BoxedStream>>,
    partial_frame: Vec<u8>,
    /// 外出调用等待应答期间到达的反向请求（ADR-0021）：先暂存，
    /// 调用返回后按序交给消费方，保证恰好一次处理。
    deferred: std::collections::VecDeque<Incoming>,
}

/// 暂存上限：守护进程侧对单条连接串行发起反向调用，正常最多 1 条在飞。
const DEFERRED_REQUEST_CAP: usize = 8;

pub struct StreamWriter {
    writer: WriteHalf<BoxedStream>,
    next_id: u64,
}

impl StreamConnection {
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (reader, writer) = tokio::io::split(Box::new(stream) as BoxedStream);
        Self {
            reader: StreamReader {
                reader: BufReader::new(reader),
                partial_frame: Vec::new(),
                deferred: std::collections::VecDeque::new(),
            },
            writer: StreamWriter { writer, next_id: 1 },
        }
    }

    pub fn into_split(self) -> (StreamReader, StreamWriter) {
        (self.reader, self.writer)
    }

    pub async fn send_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, ClientError> {
        self.writer.send_request(method, params).await
    }

    pub async fn next_incoming(&mut self) -> Result<Incoming, ClientError> {
        self.reader.next_incoming().await
    }

    pub async fn call<F>(
        &mut self,
        method: &str,
        params: Value,
        on_notification: F,
    ) -> Result<Value, ClientError>
    where
        F: FnMut(Notification),
    {
        let id = self.writer.allocate_id();
        let request = request(method, params, id)?;
        self.call_request(request, json!(id), REQUEST_TIMEOUT, on_notification)
            .await
    }

    pub async fn call_with_timeout<F>(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        on_notification: F,
    ) -> Result<Value, ClientError>
    where
        F: FnMut(Notification),
    {
        let id = self.writer.allocate_id();
        let request = request(method, params, id)?;
        self.call_request(request, json!(id), timeout, on_notification)
            .await
    }

    /// 插件认证（ADR-0017）：把本连接绑定为对应插件主体。
    /// 必须是连接上第一个请求；`-32014`（AUTH_REJECTED）后对端会关闭连接，
    /// 后续请求按连接故障处理。[`RpcClient`] 的便捷 `call` 每请求独立连接，
    /// 因此插件主体只能经本方法绑定。
    pub async fn authenticate(&mut self, token: &str) -> Result<(), ClientError> {
        self.call(
            "authenticate",
            json!({
                "token": token,
            }),
            |_| {},
        )
        .await
        .map(|_| ())
    }

    async fn call_request<F>(
        &mut self,
        request: Value,
        expected_id: Value,
        timeout: Duration,
        mut on_notification: F,
    ) -> Result<Value, ClientError>
    where
        F: FnMut(Notification),
    {
        tokio::time::timeout(timeout, async {
            self.writer.write_request(&request).await?;
            loop {
                match self.reader.next_incoming_raw().await? {
                    Incoming::Notification(notification) => on_notification(notification),
                    Incoming::Request { id, method, params } => {
                        // 等待自身应答期间到达的反向请求：暂存，不丢帧、
                        // 不在借用 writer 的路径上应答。
                        let incoming = Incoming::Request { id, method, params };
                        if self.reader.deferred.len() >= DEFERRED_REQUEST_CAP {
                            return Err(ClientError::rpc("反向请求暂存超限"));
                        }
                        self.reader.deferred.push_back(incoming);
                    }
                    response @ Incoming::Response { .. } => {
                        return finish_response(response, &expected_id)
                    }
                }
            }
        })
        .await
        .map_err(|_| ClientError::connection("JSON-RPC 请求超时"))?
    }
}

impl StreamReader {
    pub async fn next_incoming(&mut self) -> Result<Incoming, ClientError> {
        if let Some(deferred) = self.deferred.pop_front() {
            return Ok(deferred);
        }
        self.next_incoming_raw().await
    }

    /// 绕过暂存队列直读下一帧：外出调用的等待循环用它，把到达的反向请求
    /// 压入暂存而不被自己刚压入的帧卡死。
    async fn next_incoming_raw(&mut self) -> Result<Incoming, ClientError> {
        let mut empty_frames = 0_usize;
        loop {
            let frame = read_stream_frame(&mut self.reader, &mut self.partial_frame).await?;
            if !frame.iter().all(u8::is_ascii_whitespace) {
                return decode_incoming(&frame);
            }
            empty_frames = empty_frames.saturating_add(1);
            if empty_frames > MAX_CONSECUTIVE_EMPTY_FRAMES {
                return Err(ClientError::rpc(format!(
                    "连续空白 JSON-RPC 帧超过 {MAX_CONSECUTIVE_EMPTY_FRAMES} 条"
                )));
            }
        }
    }
}

impl StreamWriter {
    pub async fn send_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, ClientError> {
        let id = self.allocate_id();
        let request = request(method, params, id)?;
        tokio::time::timeout(REQUEST_TIMEOUT, self.write_request(&request))
            .await
            .map_err(|_| ClientError::connection("写入 JSON-RPC 请求超时"))??;
        Ok(json!(id))
    }

    fn allocate_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        id
    }

    /// 以给定 id 应答一条反向请求（ADR-0021）。不分配新 id——id 必须与
    /// 守护进程出站请求一致，否则对端按未知 id 丢弃。
    pub async fn respond(
        &mut self,
        id: &Value,
        outcome: Result<Value, JsonRpcError>,
    ) -> Result<(), ClientError> {
        let mut frame = serde_json::Map::new();
        frame.insert("jsonrpc".into(), json!("2.0"));
        frame.insert("id".into(), id.clone());
        match outcome {
            Ok(result) => frame.insert("result".into(), result),
            Err(error) => frame.insert(
                "error".into(),
                serde_json::to_value(error).map_err(|error| {
                    ClientError::rpc(format!("序列化 JSON-RPC error 失败: {error}"))
                })?,
            ),
        };
        let mut encoded = serde_json::to_vec(&Value::Object(frame))
            .map_err(|error| ClientError::rpc(format!("序列化 JSON-RPC 响应失败: {error}")))?;
        if encoded.len() > MAX_FRAME_BYTES {
            return Err(ClientError::rpc(format!(
                "JSON-RPC 响应超过 {MAX_FRAME_BYTES} bytes"
            )));
        }
        encoded.push(b'\n');
        self.writer
            .write_all(&encoded)
            .await
            .map_err(|error| ClientError::connection(format!("写入 JSON-RPC 响应失败: {error}")))?;
        self.writer
            .flush()
            .await
            .map_err(|error| ClientError::connection(format!("刷新 JSON-RPC 响应失败: {error}")))
    }

    async fn write_request(&mut self, request: &Value) -> Result<(), ClientError> {
        let mut encoded = serde_json::to_vec(request)
            .map_err(|error| ClientError::rpc(format!("序列化 JSON-RPC 请求失败: {error}")))?;
        if encoded.len() > MAX_FRAME_BYTES {
            return Err(ClientError::rpc(format!(
                "JSON-RPC 请求超过 {MAX_FRAME_BYTES} bytes"
            )));
        }
        encoded.push(b'\n');
        self.writer
            .write_all(&encoded)
            .await
            .map_err(|error| ClientError::connection(format!("写入 JSON-RPC 请求失败: {error}")))?;
        self.writer
            .flush()
            .await
            .map_err(|error| ClientError::connection(format!("刷新 JSON-RPC 请求失败: {error}")))
    }
}

pub async fn connect_stream(endpoint: &Endpoint) -> Result<StreamConnection, ClientError> {
    validate_endpoint(endpoint)?;
    match endpoint {
        Endpoint::Http { .. } => Err(ClientError::connection("HTTP 传输不提供流式通知")),
        Endpoint::Unix { path } => {
            #[cfg(unix)]
            {
                let stream =
                    tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::UnixStream::connect(path))
                        .await
                        .map_err(|_| {
                            ClientError::connection(format!(
                                "连接 Unix socket 超时: {}",
                                path.display()
                            ))
                        })?
                        .map_err(|error| {
                            ClientError::connection(format!(
                                "连接 Unix socket {} 失败: {error}",
                                path.display()
                            ))
                        })?;
                Ok(StreamConnection::from_stream(stream))
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                Err(ClientError::connection(
                    "Unix socket 传输只在 Unix 平台可用",
                ))
            }
        }
        Endpoint::NamedPipe { name } => {
            #[cfg(windows)]
            {
                connect_named_pipe(name)
                    .await
                    .map(StreamConnection::from_stream)
            }
            #[cfg(not(windows))]
            {
                let _ = name;
                Err(ClientError::connection(
                    "named pipe 传输只在 Windows 平台可用",
                ))
            }
        }
    }
}

#[cfg(windows)]
async fn connect_named_pipe(
    name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, ClientError> {
    use tokio::net::windows::named_pipe::ClientOptions;

    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;

    let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
    loop {
        match ClientOptions::new().open(name) {
            Ok(client) => return Ok(client),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(ERROR_FILE_NOT_FOUND) | Some(ERROR_PIPE_BUSY)
                ) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => {
                return Err(ClientError::connection(format!(
                    "连接 named pipe {name} 失败: {error}"
                )))
            }
        }
    }
}

async fn read_stream_frame<R>(
    reader: &mut R,
    partial_frame: &mut Vec<u8>,
) -> Result<Vec<u8>, ClientError>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| ClientError::connection(format!("读取 JSON-RPC 响应失败: {error}")))?;
        if available.is_empty() {
            return Err(ClientError::connection("JSON-RPC 连接在响应前关闭"));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map(|index| index + 1).unwrap_or(available.len());
        let payload_bytes = consumed - usize::from(newline.is_some());
        if partial_frame.len().saturating_add(payload_bytes) > MAX_FRAME_BYTES {
            return Err(ClientError::rpc(format!(
                "JSON-RPC 响应超过 {MAX_FRAME_BYTES} bytes"
            )));
        }
        partial_frame.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            while matches!(partial_frame.last(), Some(b'\n' | b'\r')) {
                partial_frame.pop();
            }
            return Ok(std::mem::take(partial_frame));
        }
    }
}

fn decode_incoming(bytes: &[u8]) -> Result<Incoming, ClientError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| ClientError::rpc(format!("解析 JSON-RPC 响应失败: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| ClientError::rpc("JSON-RPC 消息必须是对象"))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(ClientError::rpc("JSON-RPC 消息缺少有效 jsonrpc=2.0"));
    }

    if let Some(method) = object.get("method") {
        if object.contains_key("result") || object.contains_key("error") {
            return Err(ClientError::rpc("JSON-RPC 请求/通知结构无效"));
        }
        let method = method
            .as_str()
            .filter(|method| !method.is_empty())
            .ok_or_else(|| ClientError::rpc("JSON-RPC 请求 method 无效"))?;
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        // id 缺省或 null 是通知；非空 id 是守护进程发起的反向请求（ADR-0021）。
        let id = object.get("id").filter(|id| !id.is_null()).cloned();
        return match id {
            Some(id) => Ok(Incoming::Request {
                id,
                method: method.to_owned(),
                params,
            }),
            None => Ok(Incoming::Notification(Notification {
                method: method.to_owned(),
                params,
            })),
        };
    }

    let id = object
        .get("id")
        .filter(|id| !id.is_null())
        .cloned()
        .ok_or_else(|| ClientError::rpc("JSON-RPC 响应缺少有效 id"))?;
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");
    if has_result == has_error {
        return Err(ClientError::rpc(
            "JSON-RPC 响应必须且只能包含 result 或 error",
        ));
    }
    let result = if has_result {
        Ok(object.get("result").cloned().unwrap_or(Value::Null))
    } else {
        let error = serde_json::from_value(
            object
                .get("error")
                .cloned()
                .expect("has_error guarantees the field"),
        )
        .map_err(|error| ClientError::rpc(format!("JSON-RPC error 对象无效: {error}")))?;
        Err(error)
    };
    Ok(Incoming::Response { id, result })
}

fn finish_response(incoming: Incoming, expected_id: &Value) -> Result<Value, ClientError> {
    match incoming {
        Incoming::Notification(_) => Err(ClientError::rpc("期待响应时收到通知")),
        Incoming::Request { .. } => Err(ClientError::rpc("期待响应时收到反向请求")),
        Incoming::Response { id, result } => {
            if &id != expected_id {
                return Err(ClientError::rpc("JSON-RPC 响应 id 与请求不匹配"));
            }
            result.map_err(ClientError::rpc_response)
        }
    }
}

// —— 账户管理（M3.7 / ADR-0015 §9）：DTO、入站校验与 typed calls ——

pub const MAX_ACCOUNT_DISPLAY_NAME_BYTES: usize = 128;
pub const MAX_ACCOUNT_ADDRESS_BYTES: usize = 320;
pub const MAX_ACCOUNT_USERNAME_BYTES: usize = 320;
pub const MAX_ACCOUNT_HOST_BYTES: usize = 253;
pub const MAX_ACCOUNT_MAILBOX_BYTES: usize = 255;
pub const MAX_ACCOUNT_PROVIDER_ID_BYTES: usize = 128;
pub const MAX_ACCOUNT_SECRET_BYTES: usize = 4096;
pub const MAX_MANAGED_PROFILES: usize = 64;

/// 来源。未知字符串按 Unknown 展示，不让整个账户页解码失败。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AccountSource {
    Managed,
    Legacy,
    #[serde(other)]
    Unknown,
}

/// 收信 endpoint 视图。security 用 String：未知模式按原文展示而非解码失败。
/// 字段全部容忍缺省（ADR-0021）：委托态 profile 只有 kind=plugin 与 provider，
/// 旧字段缺省占位，不让整个 profile 列表解码失败。
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct IncomingView {
    pub kind: String,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub security: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub mailbox: String,
    #[serde(default)]
    pub idle: bool,
    #[serde(default = "default_view_poll_secs")]
    pub poll_secs: u64,
    /// kind=plugin 时的受托插件 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

fn default_view_poll_secs() -> u64 {
    60
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct OutgoingView {
    pub kind: String,
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub security: String,
    #[serde(default)]
    pub username: String,
    /// kind=plugin 时的受托插件 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// incoming | separate | <unknown>；委托态缺省为空。
    #[serde(default)]
    pub credential: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct CredentialsView {
    /// saved | missing | unknown | shared | embedded
    pub incoming: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct RuntimeView {
    /// connecting | online | reconnecting | disabled | needs_credentials | error
    pub state: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct AccountProfileView {
    pub id: String,
    pub revision: u64,
    pub display_name: String,
    pub address: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<u64>,
    pub source: AccountSource,
    pub editable: bool,
    pub incoming: IncomingView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<OutgoingView>,
    pub credentials: CredentialsView,
    pub runtime: RuntimeView,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ProfilesResult {
    pub profiles: Vec<AccountProfileView>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ProbeOutcomeView {
    /// dns | connect | tls | auth | mailbox | capability | ready | <unknown>
    pub stage: String,
    pub ok: bool,
    #[serde(default)]
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ProbeReportView {
    pub incoming: ProbeOutcomeView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<ProbeOutcomeView>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct CreateAccountResult {
    pub profile: AccountProfileView,
    pub persisted: bool,
    pub active: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct RemoveAccountResult {
    pub archived: bool,
    pub credentials_deleted: bool,
    pub messages_retained: bool,
}

// —— 输入类型（客户端 → 服务端） ——

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum IncomingInput {
    Imap {
        server: String,
        port: u16,
        security: String,
        username: String,
        mailbox: String,
        idle: bool,
        poll_secs: u64,
    },
    Plugin {
        provider: String,
        poll_secs: u64,
    },
}

impl IncomingInput {
    pub fn imap(
        server: impl Into<String>,
        port: u16,
        security: impl Into<String>,
        username: impl Into<String>,
        mailbox: impl Into<String>,
        idle: bool,
        poll_secs: u64,
    ) -> Self {
        Self::Imap {
            server: server.into(),
            port,
            security: security.into(),
            username: username.into(),
            mailbox: mailbox.into(),
            idle,
            poll_secs,
        }
    }

    pub fn plugin(provider: impl Into<String>, poll_secs: u64) -> Self {
        Self::Plugin {
            provider: provider.into(),
            poll_secs,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum OutgoingInput {
    Smtp {
        server: String,
        port: u16,
        security: String,
        username: String,
        /// incoming | separate
        credential: String,
    },
    Plugin {
        provider: String,
    },
}

impl OutgoingInput {
    pub fn smtp(
        server: impl Into<String>,
        port: u16,
        security: impl Into<String>,
        username: impl Into<String>,
        credential: impl Into<String>,
    ) -> Self {
        Self::Smtp {
            server: server.into(),
            port,
            security: security.into(),
            username: username.into(),
            credential: credential.into(),
        }
    }

    pub fn plugin(provider: impl Into<String>) -> Self {
        Self::Plugin {
            provider: provider.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct AccountDraftInput {
    pub display_name: String,
    pub address: String,
    pub incoming: IncomingInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<OutgoingInput>,
}

/// 部分更新：`outgoing` 双层 Option（缺省=不变，None=移除，Some=替换）。
#[derive(Clone, Debug, Default, Serialize, Eq, PartialEq)]
pub struct UpdatePatchInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incoming: Option<IncomingInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<Option<OutgoingInput>>,
}

#[derive(Debug)]
pub struct AccountValidationError {
    pub field: &'static str,
    pub message: String,
}

impl fmt::Display for AccountValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} 无效: {}", self.field, self.message)
    }
}

impl std::error::Error for AccountValidationError {}

fn account_error(field: &'static str, message: impl Into<String>) -> AccountValidationError {
    AccountValidationError {
        field,
        message: message.into(),
    }
}

fn validate_security_mode(value: &str) -> Result<(), AccountValidationError> {
    if matches!(value, "implicit" | "starttls") {
        Ok(())
    } else {
        Err(account_error(
            "security",
            "受管账户只允许 implicit 或 starttls",
        ))
    }
}

fn validate_credential_sharing(value: &str) -> Result<(), AccountValidationError> {
    if matches!(value, "incoming" | "separate") {
        Ok(())
    } else {
        Err(account_error("credential", "只允许 incoming 或 separate"))
    }
}

fn validate_bounded_text(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), AccountValidationError> {
    if value.is_empty() {
        return Err(account_error(field, "不能为空"));
    }
    if value.len() > max_bytes {
        return Err(account_error(field, format!("超过 {max_bytes} 字节上限")));
    }
    if value.chars().any(char::is_control) {
        return Err(account_error(field, "不能包含控制字符"));
    }
    Ok(())
}

pub fn validate_incoming_input(incoming: &IncomingInput) -> Result<(), AccountValidationError> {
    match incoming {
        IncomingInput::Imap {
            server,
            port,
            security,
            username,
            mailbox,
            poll_secs,
            ..
        } => {
            validate_bounded_text("incoming.server", server, MAX_ACCOUNT_HOST_BYTES)?;
            if server.chars().any(|c| c.is_whitespace()) {
                return Err(account_error("incoming.server", "不能包含空白字符"));
            }
            if *port == 0 {
                return Err(account_error("incoming.port", "必须在 1..=65535"));
            }
            validate_security_mode(security)?;
            validate_bounded_text("incoming.username", username, MAX_ACCOUNT_USERNAME_BYTES)?;
            validate_bounded_text("incoming.mailbox", mailbox, MAX_ACCOUNT_MAILBOX_BYTES)?;
            if *poll_secs < 1 {
                return Err(account_error("incoming.poll_secs", "必须 ≥ 1"));
            }
        }
        IncomingInput::Plugin {
            provider,
            poll_secs,
        } => {
            validate_provider_id("incoming.provider", provider)?;
            if *poll_secs < 1 {
                return Err(account_error("incoming.poll_secs", "必须 ≥ 1"));
            }
        }
    }
    Ok(())
}

pub fn validate_outgoing_input(outgoing: &OutgoingInput) -> Result<(), AccountValidationError> {
    match outgoing {
        OutgoingInput::Smtp {
            server,
            port,
            security,
            username,
            credential,
        } => {
            validate_bounded_text("outgoing.server", server, MAX_ACCOUNT_HOST_BYTES)?;
            if server.chars().any(|c| c.is_whitespace()) {
                return Err(account_error("outgoing.server", "不能包含空白字符"));
            }
            if *port == 0 {
                return Err(account_error("outgoing.port", "必须在 1..=65535"));
            }
            validate_security_mode(security)?;
            validate_bounded_text("outgoing.username", username, MAX_ACCOUNT_USERNAME_BYTES)?;
            validate_credential_sharing(credential)?;
        }
        OutgoingInput::Plugin { provider } => {
            validate_provider_id("outgoing.provider", provider)?;
        }
    }
    Ok(())
}

fn validate_provider_id(field: &'static str, provider: &str) -> Result<(), AccountValidationError> {
    let bytes = provider.as_bytes();
    if provider.is_empty()
        || provider.len() > MAX_ACCOUNT_PROVIDER_ID_BYTES
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || provider.contains("..")
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(account_error(field, "插件 id 无效"));
    }
    Ok(())
}

/// Build the write-only credential object for a managed account request. Plugin delegated
/// directions reject secrets so callers cannot accidentally cross the provider seam.
pub fn account_credentials_input(
    draft: &AccountDraftInput,
    incoming_secret: &str,
    outgoing_secret: Option<&str>,
) -> Result<Value, ClientError> {
    let mut credentials = json!({});
    match &draft.incoming {
        IncomingInput::Imap { .. } => {
            validate_account_secret("incoming", incoming_secret)
                .map_err(|error| ClientError::rpc(error.to_string()))?;
            credentials["incoming"] = json!(incoming_secret);
        }
        IncomingInput::Plugin { .. } if !incoming_secret.is_empty() => {
            return Err(ClientError::rpc("incoming 委托给插件时不接受凭据"));
        }
        IncomingInput::Plugin { .. } => {}
    }
    match &draft.outgoing {
        Some(OutgoingInput::Smtp { credential, .. }) if credential == "separate" => {
            let secret =
                outgoing_secret.ok_or_else(|| ClientError::rpc("separate 发信凭据缺失"))?;
            validate_account_secret("outgoing", secret)
                .map_err(|error| ClientError::rpc(error.to_string()))?;
            credentials["outgoing"] = json!(secret);
        }
        Some(OutgoingInput::Plugin { .. }) if outgoing_secret.is_some() => {
            return Err(ClientError::rpc("outgoing 委托给插件时不接受凭据"));
        }
        _ => {}
    }
    Ok(credentials)
}

pub fn validate_draft_input(draft: &AccountDraftInput) -> Result<(), AccountValidationError> {
    validate_bounded_text(
        "display_name",
        &draft.display_name,
        MAX_ACCOUNT_DISPLAY_NAME_BYTES,
    )?;
    validate_bounded_text("address", &draft.address, MAX_ACCOUNT_ADDRESS_BYTES)?;
    let Some((local, domain)) = draft.address.split_once('@') else {
        return Err(account_error("address", "不是有效邮箱地址"));
    };
    if local.is_empty() || domain.is_empty() {
        return Err(account_error("address", "不是有效邮箱地址"));
    }
    validate_incoming_input(&draft.incoming)?;
    if let Some(outgoing) = &draft.outgoing {
        validate_outgoing_input(outgoing)?;
    }
    Ok(())
}

pub fn validate_account_secret(
    field: &'static str,
    secret: &str,
) -> Result<(), AccountValidationError> {
    if secret.is_empty() {
        return Err(account_error(field, "秘密不能为空"));
    }
    if secret.len() > MAX_ACCOUNT_SECRET_BYTES {
        return Err(account_error(
            field,
            format!("秘密超过 {MAX_ACCOUNT_SECRET_BYTES} 字节上限"),
        ));
    }
    Ok(())
}

/// 与服务端一致的响应入站校验：数量与字段长度受限；未知枚举按原文展示。
pub fn validate_profiles_result(result: &ProfilesResult) -> Result<(), AccountValidationError> {
    if result.profiles.len() > MAX_MANAGED_PROFILES * 2 {
        // legacy + 受管合计仍应有界；超限视为服务端异常。
        return Err(account_error("profiles", "profile 数量异常"));
    }
    for profile in &result.profiles {
        validate_bounded_text(
            "profile.display_name",
            &profile.display_name,
            MAX_ACCOUNT_DISPLAY_NAME_BYTES,
        )?;
        validate_bounded_text(
            "profile.address",
            &profile.address,
            MAX_ACCOUNT_ADDRESS_BYTES,
        )?;
        if profile.incoming.kind == "plugin" {
            let provider = profile
                .incoming
                .provider
                .as_deref()
                .ok_or_else(|| account_error("incoming.provider", "插件账户缺少 provider"))?;
            validate_provider_id("incoming.provider", provider)?;
            if profile.incoming.poll_secs < 1 {
                return Err(account_error("incoming.poll_secs", "必须 ≥ 1"));
            }
        } else {
            validate_incoming_view(&profile.incoming)?;
        }
    }
    Ok(())
}

fn validate_incoming_view(incoming: &IncomingView) -> Result<(), AccountValidationError> {
    validate_bounded_text("incoming.server", &incoming.server, MAX_ACCOUNT_HOST_BYTES)?;
    validate_bounded_text(
        "incoming.username",
        &incoming.username,
        MAX_ACCOUNT_USERNAME_BYTES,
    )?;
    validate_bounded_text(
        "incoming.mailbox",
        &incoming.mailbox,
        MAX_ACCOUNT_MAILBOX_BYTES,
    )
}

impl RpcClient {
    pub async fn list_account_profiles(
        &self,
        include_archived: bool,
    ) -> Result<ProfilesResult, ClientError> {
        let result: ProfilesResult = self
            .call_typed(
                "listAccountProfiles",
                json!({ "include_archived": include_archived }),
            )
            .await?;
        validate_profiles_result(&result)
            .map_err(|error| ClientError::rpc(format!("listAccountProfiles 响应无效: {error}")))?;
        Ok(result)
    }

    pub async fn probe_account(
        &self,
        draft: &AccountDraftInput,
        incoming_secret: &str,
        outgoing_secret: Option<&str>,
    ) -> Result<ProbeReportView, ClientError> {
        validate_draft_input(draft).map_err(|error| ClientError::rpc(error.to_string()))?;
        let credentials = account_credentials_input(draft, incoming_secret, outgoing_secret)?;
        self.call_typed(
            "probeAccount",
            json!({ "draft": draft, "credentials": credentials }),
        )
        .await
    }

    pub async fn create_account(
        &self,
        draft: &AccountDraftInput,
        incoming_secret: &str,
        outgoing_secret: Option<&str>,
    ) -> Result<CreateAccountResult, ClientError> {
        validate_draft_input(draft).map_err(|error| ClientError::rpc(error.to_string()))?;
        let credentials = account_credentials_input(draft, incoming_secret, outgoing_secret)?;
        self.call_typed(
            "createAccount",
            json!({ "draft": draft, "credentials": credentials }),
        )
        .await
    }

    pub async fn update_account(
        &self,
        id: &str,
        expected_revision: u64,
        patch: &UpdatePatchInput,
        secrets: AccountSecretUpdate<'_>,
    ) -> Result<AccountProfileView, ClientError> {
        if id.is_empty() {
            return Err(ClientError::rpc("id 不能为空"));
        }
        if expected_revision < 1 {
            return Err(ClientError::rpc("expected_revision 必须 ≥ 1"));
        }
        let mut params = json!({
            "id": id,
            "expected_revision": expected_revision,
            "patch": patch,
        });
        match (secrets.incoming, secrets.outgoing) {
            (None, None) => {}
            _ => {
                let mut credentials = json!({});
                if let Some(secret) = secrets.incoming {
                    validate_account_secret("incoming", secret)
                        .map_err(|error| ClientError::rpc(error.to_string()))?;
                    credentials["incoming"] = json!(secret);
                }
                if let Some(secret) = secrets.outgoing {
                    validate_account_secret("outgoing", secret)
                        .map_err(|error| ClientError::rpc(error.to_string()))?;
                    credentials["outgoing"] = json!(secret);
                }
                params["credentials"] = credentials;
            }
        }
        self.call_typed("updateAccount", params).await
    }

    pub async fn set_account_enabled(
        &self,
        id: &str,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<AccountProfileView, ClientError> {
        if id.is_empty() || expected_revision < 1 {
            return Err(ClientError::rpc("id/expected_revision 无效"));
        }
        self.call_typed(
            "setAccountEnabled",
            json!({ "id": id, "expected_revision": expected_revision, "enabled": enabled }),
        )
        .await
    }

    pub async fn remove_account(
        &self,
        id: &str,
        expected_revision: u64,
    ) -> Result<RemoveAccountResult, ClientError> {
        if id.is_empty() || expected_revision < 1 {
            return Err(ClientError::rpc("id/expected_revision 无效"));
        }
        self.call_typed(
            "removeAccount",
            json!({ "id": id, "expected_revision": expected_revision }),
        )
        .await
    }

    pub async fn send_from_account(
        &self,
        account: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> Result<(), ClientError> {
        self.send_from_account_with_address(account, to, subject, body, None)
            .await
    }

    /// ADR-0022 additive 入口；保留四参数 `send_from_account` 的源码兼容。
    pub async fn send_from_account_with_address(
        &self,
        account: &str,
        to: &str,
        subject: &str,
        body: &str,
        from_address: Option<&str>,
    ) -> Result<(), ClientError> {
        #[derive(Deserialize)]
        struct SendResult {
            ok: bool,
        }

        if account.is_empty() {
            return Err(ClientError::rpc("account 不能为空"));
        }
        validate_send_field(SendField::To, to)
            .map_err(|error| ClientError::rpc(error.to_string()))?;
        validate_send_field(SendField::Subject, subject)
            .map_err(|error| ClientError::rpc(error.to_string()))?;
        validate_send_field(SendField::Body, body)
            .map_err(|error| ClientError::rpc(error.to_string()))?;
        let payload = serde_json::to_value(SendFromAccountRequest {
            account: account.into(),
            to: to.into(),
            subject: subject.into(),
            body: body.into(),
            from_address: from_address.map(str::to_owned),
        })
        .map_err(|_| ClientError::rpc("sendFromAccount 请求序列化失败"))?;
        let result: SendResult = self.call_typed("sendFromAccount", payload).await?;
        if !result.ok {
            return Err(ClientError::rpc("sendFromAccount 响应缺少 ok=true"));
        }
        Ok(())
    }

    /// 受管账户的别名/一次性地址列表（M4-06，ADR-0022）。
    pub async fn list_addresses(
        &self,
        account: Option<&str>,
        include_inactive: bool,
    ) -> Result<Vec<AddressView>, ClientError> {
        let mut payload = json!({ "include_inactive": include_inactive });
        if let Some(account) = account {
            payload["account"] = json!(account);
        }
        let wrapped: AddressesResult = self.call_typed("listAddresses", payload).await?;
        Ok(wrapped.addresses)
    }

    /// 有界地址页；服务端上限 1000。旧的全量 `list_addresses` 保持兼容。
    pub async fn list_addresses_page(
        &self,
        account: Option<&str>,
        include_inactive: bool,
        limit: u64,
        offset: u64,
    ) -> Result<AddressesResult, ClientError> {
        if !(1..=1000).contains(&limit) {
            return Err(ClientError::rpc("limit 必须是 1 到 1000 的整数"));
        }
        let mut payload = json!({
            "include_inactive": include_inactive,
            "limit": limit,
            "offset": offset,
        });
        if let Some(account) = account {
            payload["account"] = json!(account);
        }
        self.call_typed("listAddresses", payload).await
    }

    /// 创建别名或一次性地址；disposable 可省略 address（daemon 生成）。
    pub async fn create_address(
        &self,
        account: &str,
        kind: &str,
        address: Option<&str>,
        label: Option<&str>,
        expires_secs: Option<u64>,
    ) -> Result<AddressView, ClientError> {
        let mut payload = json!({ "account": account, "kind": kind });
        if let Some(address) = address {
            payload["address"] = json!(address);
        }
        if let Some(label) = label {
            payload["label"] = json!(label);
        }
        if let Some(expires_secs) = expires_secs {
            payload["expires_secs"] = json!(expires_secs);
        }
        self.call_typed("createAddress", payload).await
    }

    /// 启用/停用或改名；state 只接受 "active" | "disabled"。
    pub async fn update_address(
        &self,
        id: &str,
        state: Option<&str>,
        label: Option<&str>,
    ) -> Result<AddressView, ClientError> {
        let mut payload = json!({ "id": id });
        if let Some(state) = state {
            payload["state"] = json!(state);
        }
        if let Some(label) = label {
            payload["label"] = json!(label);
        }
        self.call_typed("updateAddress", payload).await
    }

    pub async fn remove_address(&self, id: &str) -> Result<(), ClientError> {
        #[derive(Deserialize)]
        struct RemoveResult {
            ok: bool,
        }
        let result: RemoveResult = self
            .call_typed("removeAddress", json!({ "id": id }))
            .await?;
        if !result.ok {
            return Err(ClientError::rpc("removeAddress 响应缺少 ok=true"));
        }
        Ok(())
    }
}

/// updateAccount 携带的新秘密（None = 不修改）。
#[derive(Clone, Copy, Debug, Default)]
pub struct AccountSecretUpdate<'a> {
    pub incoming: Option<&'a str>,
    pub outgoing: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt as _;

    fn sample_draft() -> AccountDraftInput {
        AccountDraftInput {
            display_name: "Main".into(),
            address: "user@example.test".into(),
            incoming: IncomingInput::imap(
                "imap.example.test",
                993,
                "implicit",
                "user@example.test",
                "INBOX",
                true,
                60,
            ),
            outgoing: Some(OutgoingInput::smtp(
                "smtp.example.test",
                587,
                "starttls",
                "user@example.test",
                "incoming",
            )),
        }
    }

    #[test]
    fn upload_reference_is_distinct_and_legacy_root_part_keeps_zero_index() {
        let old = DraftAttachment { account: "a".into(), folder: "INBOX".into(), uid: "1".into(), ..Default::default() };
        let value = serde_json::to_value(&old).unwrap();
        assert_eq!(value.get("part_index"), Some(&json!(0)));
        assert_eq!(value.as_object().unwrap().len(), 6);
        let new = DraftAttachment { draft_id: Some("d".into()), upload_id: Some("u".into()), ..Default::default() };
        assert_eq!(serde_json::to_value(new).unwrap(), json!({"draft_id":"d", "upload_id":"u"}));
        let invalid = DraftAttachment { upload_id: Some("u".into()), ..Default::default() };
        assert!(serde_json::to_value(invalid).is_err());
    }

    #[test]
    fn delegated_profile_views_decode_tolerantly() {
        // ADR-0021 §8：委托态 profile 只有 kind/provider；缺省字段不让
        // 整个 profile 列表解码失败。
        let incoming: IncomingView = serde_json::from_value(json!({
            "kind": "plugin",
            "provider": "org.example.transport",
        }))
        .unwrap();
        assert_eq!(incoming.kind, "plugin");
        assert_eq!(incoming.provider.as_deref(), Some("org.example.transport"));
        assert_eq!(incoming.server, "");
        assert_eq!(incoming.port, 0);
        assert_eq!(incoming.poll_secs, 60);

        let outgoing: OutgoingView = serde_json::from_value(json!({
            "kind": "plugin",
            "provider": "org.example.relay",
        }))
        .unwrap();
        assert_eq!(outgoing.kind, "plugin");
        assert_eq!(outgoing.provider.as_deref(), Some("org.example.relay"));

        // 内置视图不受影响：provider 缺省为 None。
        let imap: IncomingView = serde_json::from_value(json!({
            "kind": "imap",
            "server": "imap.example.test",
            "port": 993,
            "security": "implicit",
            "username": "me@example.test",
            "mailbox": "INBOX",
            "idle": true,
            "poll_secs": 60,
        }))
        .unwrap();
        assert_eq!(imap.provider, None);
    }

    #[test]
    fn draft_validation_enforces_caps_and_enums() {
        assert!(validate_draft_input(&sample_draft()).is_ok());

        let mut draft = sample_draft();
        draft.display_name = "x".repeat(MAX_ACCOUNT_DISPLAY_NAME_BYTES + 1);
        assert!(validate_draft_input(&draft).is_err());
        draft.display_name = "Main".into();

        draft.address = "not-an-address".into();
        assert!(validate_draft_input(&draft).is_err());
        draft.address = "user@example.test".into();

        if let IncomingInput::Imap { security, .. } = &mut draft.incoming {
            *security = "none".into();
        } else {
            panic!("sample incoming must be IMAP");
        }
        assert!(validate_draft_input(&draft).is_err(), "受管账户禁止明文");
        if let IncomingInput::Imap { security, .. } = &mut draft.incoming {
            *security = "implicit".into();
        }

        if let IncomingInput::Imap { port, .. } = &mut draft.incoming {
            *port = 0;
        } else {
            panic!("sample incoming must be IMAP");
        }
        assert!(validate_draft_input(&draft).is_err());
        if let IncomingInput::Imap { port, .. } = &mut draft.incoming {
            *port = 993;
        }

        let Some(OutgoingInput::Smtp { credential, .. }) = draft.outgoing.as_mut() else {
            panic!("sample outgoing must be SMTP");
        };
        *credential = "bogus".into();
        assert!(validate_draft_input(&draft).is_err());
        let Some(OutgoingInput::Smtp { credential, .. }) = draft.outgoing.as_mut() else {
            panic!("sample outgoing must be SMTP");
        };
        *credential = "separate".into();
        assert!(
            validate_account_secret("incoming", &"x".repeat(MAX_ACCOUNT_SECRET_BYTES + 1)).is_err()
        );
    }

    #[test]
    fn draft_serialization_matches_server_schema() {
        let value = serde_json::to_value(sample_draft()).unwrap();
        assert_eq!(value["incoming"]["kind"], "imap");
        assert_eq!(value["outgoing"]["kind"], "smtp");
        assert_eq!(value["outgoing"]["credential"], "incoming");
        assert_eq!(value["display_name"], "Main");
    }

    #[test]
    fn plugin_draft_serializes_without_transport_credentials() {
        let draft = AccountDraftInput {
            display_name: "NodeSeek".into(),
            address: "user@seek.li".into(),
            incoming: IncomingInput::plugin("org.nodeseek.email", 30),
            outgoing: Some(OutgoingInput::plugin("org.nodeseek.email")),
        };
        assert!(validate_draft_input(&draft).is_ok());
        assert_eq!(
            account_credentials_input(&draft, "", None).unwrap(),
            json!({})
        );
        assert!(account_credentials_input(&draft, "must-not-cross-seam", None).is_err());
        assert!(account_credentials_input(&draft, "", Some("must-not-cross-seam")).is_err());
        assert_eq!(
            serde_json::to_value(&draft).unwrap(),
            json!({
                "display_name": "NodeSeek",
                "address": "user@seek.li",
                "incoming": {
                    "kind": "plugin",
                    "provider": "org.nodeseek.email",
                    "poll_secs": 30,
                },
                "outgoing": {
                    "kind": "plugin",
                    "provider": "org.nodeseek.email",
                },
            })
        );

        let mut invalid = draft.clone();
        invalid.incoming = IncomingInput::plugin("", 30);
        assert_eq!(
            validate_draft_input(&invalid).unwrap_err().field,
            "incoming.provider"
        );
        invalid.incoming = IncomingInput::plugin("org.nodeseek.email", 0);
        assert_eq!(
            validate_draft_input(&invalid).unwrap_err().field,
            "incoming.poll_secs"
        );
    }

    #[test]
    fn profile_views_tolerate_unknown_status_strings() {
        let raw = json!({
            "profiles": [{
                "id": "acct_abcdefghijklmnopqrstuvwxyz23",
                "revision": 3,
                "display_name": "Main",
                "address": "user@example.test",
                "enabled": true,
                "source": "some-future-source",
                "editable": true,
                "incoming": {
                    "kind": "imap",
                    "server": "imap.example.test",
                    "port": 993,
                    "security": "quantum",
                    "username": "u@example.test",
                    "mailbox": "INBOX",
                    "idle": true,
                    "poll_secs": 60
                },
                "credentials": { "incoming": "saved" },
                "runtime": { "state": "warping", "detail": "" }
            }]
        });
        let result: ProfilesResult = serde_json::from_value(raw).unwrap();
        assert_eq!(result.profiles[0].source, AccountSource::Unknown);
        assert_eq!(result.profiles[0].runtime.state, "warping");
        assert_eq!(result.profiles[0].incoming.security, "quantum");
        assert!(validate_profiles_result(&result).is_ok());
    }

    #[test]
    fn plugin_profile_views_allow_empty_imap_fields() {
        let raw = json!({
            "profiles": [{
                "id": "acct_abcdefghijklmnopqrstuvwxyz23",
                "revision": 1,
                "display_name": "NodeSeek",
                "address": "user@seek.li",
                "enabled": true,
                "source": "managed",
                "editable": true,
                "incoming": {
                    "kind": "plugin",
                    "poll_secs": 60,
                    "provider": "org.nodeseek.email"
                },
                "outgoing": {
                    "kind": "plugin",
                    "provider": "org.nodeseek.email"
                },
                "credentials": { "incoming": "shared", "outgoing": "shared" },
                "runtime": { "state": "online", "detail": "" }
            }]
        });
        let result: ProfilesResult = serde_json::from_value(raw).unwrap();
        assert!(validate_profiles_result(&result).is_ok());
    }

    #[test]
    fn token_is_32_random_bytes_as_hex() {
        let token = generate_token().unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn endpoint_normalization_only_adds_rpc_once() {
        assert_eq!(
            normalize_url("http://127.0.0.1:7394").unwrap(),
            "http://127.0.0.1:7394/rpc"
        );
        assert_eq!(
            normalize_url("http://127.0.0.1:7394/rpc").unwrap(),
            "http://127.0.0.1:7394/rpc"
        );
        assert!(normalize_url("http://example.com").is_err());
        assert!(normalize_url("https://127.0.0.1:7394").is_err());
        assert!(normalize_url("http://user:secret@127.0.0.1:7394").is_err());
        assert!(normalize_url("http://127.0.0.1:7394?token=secret").is_err());
        assert!(normalize_url("http://127.0.0.1:7394#secret").is_err());
    }

    #[test]
    fn endpoint_debug_redacts_http_token() {
        let endpoint = Endpoint::Http {
            url: "http://127.0.0.1:7394/rpc".into(),
            token: "top-secret".into(),
        };
        let output = format!("{endpoint:?}");
        assert!(output.contains("[redacted]"));
        assert!(!output.contains("top-secret"));
    }

    #[test]
    fn direct_http_endpoint_cannot_bypass_loopback_boundary() {
        let error = RpcClient::new(Endpoint::Http {
            url: "http://192.0.2.1/rpc".into(),
            token: "top-secret".into(),
        })
        .err()
        .expect("non-loopback endpoint must be rejected");
        assert_eq!(error.kind, ErrorKind::Connection);
        assert!(error.message.contains("只允许回环地址"));
    }

    #[test]
    fn endpoint_environment_names_follow_the_brand() {
        assert_eq!(API_PIPE_ENV, format!("{}_PIPE", maild_brand::ENV_PREFIX));
        assert_eq!(API_SOCKET_ENV, maild_brand::SOCKET_ENV);
        assert_eq!(API_URL_ENV, maild_brand::URL_ENV);
        assert_eq!(API_TOKEN_ENV, maild_brand::TOKEN_ENV);
        assert_eq!(PLUGIN_TOKEN_ENV, maild_brand::PLUGIN_TOKEN_ENV);
        assert_eq!(DEFAULT_FRONTEND, maild_brand::BUNDLED_FRONTEND_ID);
    }

    #[test]
    fn explicit_endpoint_ignores_conflicting_environment_endpoints() {
        let endpoint = discover_endpoint_with_environment(
            &DiscoveryOptions {
                url: Some("http://127.0.0.1:7394".into()),
                ..DiscoveryOptions::default()
            },
            EndpointEnvironment {
                socket: Some(PathBuf::from(maild_brand::SOCKET_FILE_NAME)),
                pipe: Some(format!(r"\\.\pipe\{}", maild_brand::SLUG)),
                token: Some("t".repeat(32)),
                ..EndpointEnvironment::default()
            },
        )
        .unwrap();
        assert!(matches!(endpoint, Endpoint::Http { .. }));

        let error = discover_endpoint_with_environment(
            &DiscoveryOptions::default(),
            EndpointEnvironment {
                url: Some("http://127.0.0.1:7394".into()),
                socket: Some(Path::new("/tmp").join(maild_brand::SOCKET_FILE_NAME)),
                ..EndpointEnvironment::default()
            },
        )
        .unwrap_err();
        assert!(error.message.contains("最多只能设置一个"));
    }

    #[test]
    fn config_discovery_honors_disabled_api() {
        let result = api_config_from_toml(
            r#"
                [plugins.api]
                enabled = false
                [plugins.api.config]
                transport = "http"
            "#,
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn config_derived_relative_socket_uses_managed_runtime_directory() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join(maild_brand::CONFIG_FILE_NAME);
        std::fs::write(
            &config_path,
            format!(
                "[plugins.api.config]\ntransport = \"unix\"\nlisten = \"{}\"\n",
                maild_brand::SOCKET_FILE_NAME
            ),
        )
        .unwrap();
        let endpoint = discover_endpoint_with_environment(
            &DiscoveryOptions {
                config_path: Some(config_path),
                ..DiscoveryOptions::default()
            },
            EndpointEnvironment::default(),
        )
        .unwrap();
        let expected = maild_paths::AppPaths::discover()
            .unwrap()
            .runtime_path(Path::new(maild_brand::SOCKET_FILE_NAME))
            .unwrap();
        assert!(matches!(endpoint, Endpoint::Unix { path } if path == expected));
    }

    #[test]
    fn api_config_parser_matches_loader_shape() {
        let config = api_config_from_toml(
            r#"
                [plugins.api]
                enabled = true
                [plugins.api.config]
                transport = "http"
                listen = "127.0.0.1:9000"
                bearer_token = "fixture-token"
            "#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.transport, "http");
        assert_eq!(config.listen, "127.0.0.1:9000");
        assert_eq!(config.bearer_token.as_deref(), Some("fixture-token"));

        let secret = "do-not-render-this-token";
        let error =
            api_config_from_toml(&format!("[plugins.api.config]\nbearer_token = {secret}\n"))
                .err()
                .expect("malformed token line must fail");
        assert!(error.message.contains("TOML 解析失败"));
        assert!(!error.message.contains(secret));
    }

    #[test]
    fn config_reader_rejects_non_files_and_oversized_files() {
        assert_eq!(
            read_config_file(&std::env::temp_dir()).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "maild-client-config-{}-{suffix}.toml",
            std::process::id()
        ));
        std::fs::write(&path, vec![b'x'; MAX_CONFIG_BYTES + 1]).unwrap();
        assert_eq!(
            read_config_file(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn known_notifications_decode_to_typed_events() {
        let notification = Notification {
            method: "account/state".into(),
            params: json!({
                "account": "personal",
                "state": "online",
                "detail": ""
            }),
        };
        assert_eq!(
            notification.into_server_notification().unwrap(),
            ServerNotification::AccountState(AccountState {
                account: "personal".into(),
                state: "online".into(),
                detail: String::new(),
            })
        );

        let intent = Notification {
            method: "frontend/intent".into(),
            params: json!({ "frontend": maild_brand::BUNDLED_FRONTEND_ID }),
        };
        assert_eq!(
            intent.into_server_notification().unwrap(),
            ServerNotification::FrontendIntent {
                frontend: maild_brand::BUNDLED_FRONTEND_ID.into()
            }
        );
        assert!(Notification {
            method: "frontend/intent".into(),
            params: json!({ "frontend": 7 }),
        }
        .into_server_notification()
        .is_err());

        let stopping = Notification {
            method: "agent/stopping".into(),
            params: json!({}),
        };
        assert_eq!(
            stopping.into_server_notification().unwrap(),
            ServerNotification::AgentStopping
        );

        let unknown = Notification {
            method: "plugin/custom".into(),
            params: json!({ "future": true }),
        };
        assert!(matches!(
            unknown.into_server_notification().unwrap(),
            ServerNotification::Unknown(notification)
                if notification.method == "plugin/custom"
        ));
    }

    #[test]
    fn agent_stopping_frame_decodes_to_typed_notification() {
        let frame = br#"{"jsonrpc":"2.0","method":"agent/stopping","params":{}}"#;
        let Incoming::Notification(notification) = decode_incoming(frame).unwrap() else {
            panic!("agent/stopping 应解码为通知帧");
        };
        assert_eq!(
            notification.into_server_notification().unwrap(),
            ServerNotification::AgentStopping
        );
    }

    #[test]
    fn rpc_result_null_is_valid() {
        assert_eq!(
            decode_incoming(b"{\"jsonrpc\":\"2.0\",\"result\":null,\"id\":1}").unwrap(),
            Incoming::Response {
                id: json!(1),
                result: Ok(Value::Null),
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_call_delivers_notification_before_response() {
        let (client, server) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            let mut read = BufReader::new(read);
            let mut line = String::new();
            read.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            write
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"account/state\",\"params\":{\"state\":\"online\"}}\n")
                .await
                .unwrap();
            write
                .write_all(
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"result\":{{\"message_count\":0}},\"id\":{}}}\n",
                        request["id"]
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let mut notifications = Vec::new();
        let result = StreamConnection::from_stream(client)
            .call("status", json!({}), |notification| {
                notifications.push(notification)
            })
            .await
            .unwrap();
        assert_eq!(result["message_count"], 0);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].method, "account/state");
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn persistent_stream_exposes_out_of_order_responses() {
        let (client, server) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            let mut read = BufReader::new(read);
            let mut first = String::new();
            let mut second = String::new();
            read.read_line(&mut first).await.unwrap();
            read.read_line(&mut second).await.unwrap();
            let first: Value = serde_json::from_str(&first).unwrap();
            let second: Value = serde_json::from_str(&second).unwrap();
            write
                .write_all(
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"result\":\"second\",\"id\":{}}}\n",
                        second["id"]
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            write
                .write_all(
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"result\":\"first\",\"id\":{}}}\n",
                        first["id"]
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let mut connection = StreamConnection::from_stream(client);
        let first_id = connection.send_request("first", json!({})).await.unwrap();
        let second_id = connection.send_request("second", json!({})).await.unwrap();
        assert_eq!(first_id, json!(1));
        assert_eq!(second_id, json!(2));
        assert!(matches!(
            connection.next_incoming().await.unwrap(),
            Incoming::Response { id, result: Ok(result) }
                if id == second_id && result == json!("second")
        ));
        assert!(matches!(
            connection.next_incoming().await.unwrap(),
            Incoming::Response { id, result: Ok(result) }
                if id == first_id && result == json!("first")
        ));
        server.await.unwrap();
    }

    #[test]
    fn decode_distinguishes_reverse_request_from_notification() {
        // ADR-0021：带非空 id 的 method 帧是守护进程发起的反向请求。
        let request = decode_incoming(
            br#"{"jsonrpc":"2.0","id":7,"method":"transport/send","params":{"to":"a@b.test"}}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            Incoming::Request { ref id, ref method, .. }
                if id == &json!(7) && method == "transport/send"
        ));

        // id 缺省或 null 仍是通知（与守护进程 parse_request 对称）。
        for frame in [
            &b"{\"jsonrpc\":\"2.0\",\"method\":\"account/state\",\"params\":{}}"[..],
            b"{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"account/state\",\"params\":{}}",
        ] {
            let notification = decode_incoming(frame).unwrap();
            assert!(matches!(
                notification,
                Incoming::Notification(Notification { method, .. }) if method == "account/state"
            ));
        }

        // method + result/error 是结构错误。
        assert!(decode_incoming(br#"{"jsonrpc":"2.0","id":1,"method":"x","result":{}}"#).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_call_defers_inbound_request_until_after_response() {
        let (client, server) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            let mut read = BufReader::new(read);
            let mut line = String::new();
            read.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            write
                .write_all(
                    br#"{"jsonrpc":"2.0","id":99,"method":"transport/probe","params":{"account":"acct_x","direction":"outgoing"}}
"#,
                )
                .await
                .unwrap();
            write
                .write_all(
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"result\":{{}},\"id\":{}}}\n",
                        request["id"]
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let mut connection = StreamConnection::from_stream(client);
        connection.call("whoami", json!({}), |_| {}).await.unwrap();
        // 反向请求在 call 返回后按序交付，恰好一次。
        assert!(matches!(
            connection.next_incoming().await.unwrap(),
            Incoming::Request { id, method, .. } if id == json!(99) && method == "transport/probe"
        ));
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writer_respond_answers_inbound_request_with_matching_id() {
        let (client, server) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            write
                .write_all(
                    br#"{"jsonrpc":"2.0","id":"srv-1","method":"transport/send","params":{}}"#,
                )
                .await
                .unwrap();
            let mut read = BufReader::new(read);
            let mut line = String::new();
            read.read_line(&mut line).await.unwrap();
            line
        });

        let (_, mut writer) = StreamConnection::from_stream(client).into_split();
        writer
            .respond(
                &json!("srv-1"),
                Err(JsonRpcError {
                    code: -32020,
                    message: "upstream refused".into(),
                    data: None,
                }),
            )
            .await
            .unwrap();
        let reply: Value = serde_json::from_str(&server.await.unwrap()).unwrap();
        assert_eq!(reply["id"], json!("srv-1"));
        assert_eq!(reply["error"]["code"], -32020);
        assert_eq!(reply["error"]["message"], "upstream refused");
        assert!(reply.get("result").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn split_stream_reads_idle_notification_while_writer_remains_available() {
        let (client, server) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            write
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"account/state\",\"params\":{}}\n")
                .await
                .unwrap();
            let mut read = BufReader::new(read);
            let mut line = String::new();
            read.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            write
                .write_all(
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"result\":{{}},\"id\":{}}}\n",
                        request["id"]
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let (mut reader, mut writer) = StreamConnection::from_stream(client).into_split();
        let reader_task = tokio::spawn(async move {
            let notification = reader.next_incoming().await.unwrap();
            let response = reader.next_incoming().await.unwrap();
            (notification, response)
        });
        let id = writer.send_request("status", json!({})).await.unwrap();
        let (notification, response) = reader_task.await.unwrap();
        assert!(matches!(
            notification,
            Incoming::Notification(Notification { method, .. }) if method == "account/state"
        ));
        assert!(matches!(
            response,
            Incoming::Response { id: response_id, result: Ok(_) } if response_id == id
        ));
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_reader_preserves_partial_frame_when_read_is_cancelled() {
        let (client, mut server) = tokio::io::duplex(4096);
        let (continue_tx, continue_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            server.write_all(b"{\"jsonrpc\":\"2.0\",").await.unwrap();
            continue_rx.await.unwrap();
            server
                .write_all(b"\"method\":\"account/state\",\"params\":{}}\n")
                .await
                .unwrap();
        });

        let (mut reader, _) = StreamConnection::from_stream(client).into_split();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), reader.next_incoming())
                .await
                .is_err()
        );
        continue_tx.send(()).unwrap();
        assert!(matches!(
            reader.next_incoming().await.unwrap(),
            Incoming::Notification(Notification { method, .. }) if method == "account/state"
        ));
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_rejects_excessive_empty_frames() {
        let (client, mut server) = tokio::io::duplex(4096);
        server
            .write_all("\n".repeat(MAX_CONSECUTIVE_EMPTY_FRAMES + 1).as_bytes())
            .await
            .unwrap();
        let (mut reader, _) = StreamConnection::from_stream(client).into_split();
        let error = reader.next_incoming().await.unwrap_err();
        assert!(error.message.contains("空白 JSON-RPC 帧"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_call_rejects_mismatched_response_id() {
        let (client, mut server) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let mut request = [0_u8; 1024];
            let _ = server.read(&mut request).await.unwrap();
            server
                .write_all(b"{\"jsonrpc\":\"2.0\",\"result\":{},\"id\":999}\n")
                .await
                .unwrap();
        });
        let error = StreamConnection::from_stream(client)
            .call("status", json!({}), |_| {})
            .await
            .unwrap_err();
        assert!(error.message.contains("id 与请求不匹配"));
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_timeout_covers_request_write() {
        let (client, _server) = tokio::io::duplex(1);
        let error = StreamConnection::from_stream(client)
            .call_with_timeout(
                "send",
                json!({ "body": "x".repeat(4096) }),
                Duration::from_millis(20),
                |_| {},
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Connection);
        assert!(error.message.contains("请求超时"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_rejects_oversized_response_frame() {
        let (client, mut server) = tokio::io::duplex(MAX_FRAME_BYTES + 4096);
        let server = tokio::spawn(async move {
            server
                .write_all(&vec![b'x'; MAX_FRAME_BYTES + 1])
                .await
                .unwrap();
            server.write_all(b"\n").await.unwrap();
        });
        let error = StreamConnection::from_stream(client)
            .next_incoming()
            .await
            .unwrap_err();
        assert!(error.message.contains("响应超过"));
        server.await.unwrap();
    }

    #[test]
    fn send_validation_matches_the_protocol_boundaries() {
        let valid = SendRequest {
            from: "sender@example.test".into(),
            to: "recipient@example.test".into(),
            subject: "plain text".into(),
            body: "hello\n.world".into(),
        };
        assert_eq!(validate_send_request(&valid), Ok(()));

        for (field, value) in [
            (SendField::From, "sender name@example.test"),
            (SendField::To, "recipient@example.test\r\nBcc:x@y"),
            (SendField::Subject, "subject\nBcc:x@y"),
            (SendField::Body, "body\0tail"),
        ] {
            assert_eq!(validate_send_field(field, value).unwrap_err().field, field);
        }

        assert!(validate_send_field(SendField::From, &format!("{}@b", "a".repeat(320))).is_err());
        assert!(validate_send_field(SendField::Subject, &"x".repeat(999)).is_err());
        assert!(validate_send_field(SendField::Body, &"x".repeat(MAX_BODY_BYTES + 1)).is_err());
    }

    #[test]
    fn rules_validation_matches_runtime_settings_boundaries() {
        let valid = RulesSettings {
            show_sensitive: true,
            silent_keywords: vec!["验证码".into()],
            quiet_hours: vec![QuietHours {
                start: "22:00".into(),
                end: "07:00".into(),
            }],
            timezone_offset_minutes: 480,
            notify_new_mail: false,
            muted_accounts: Vec::new(),
        };
        assert_eq!(validate_rules_settings(&valid), Ok(()));
        assert_eq!(
            validate_settings_result(
                &SettingsResult {
                    rules: valid.clone(),
                    persisted: Some(false),
                },
                true,
            ),
            Ok(())
        );

        let mut invalid = valid.clone();
        invalid.silent_keywords[0] = "x".repeat(MAX_SILENT_KEYWORD_BYTES + 1);
        assert_eq!(
            validate_rules_settings(&invalid).unwrap_err().field,
            RulesField::SilentKeyword(0)
        );

        let mut invalid = valid.clone();
        invalid.quiet_hours[0].end = "22:00".into();
        assert_eq!(
            validate_rules_settings(&invalid).unwrap_err().field,
            RulesField::QuietHoursEnd(0)
        );

        let patch = RulesPatch {
            timezone_offset_minutes: Some(MAX_TIMEZONE_OFFSET_MINUTES + 1),
            ..Default::default()
        };
        assert_eq!(
            validate_rules_patch(&patch).unwrap_err().field,
            RulesField::TimezoneOffsetMinutes
        );

        // ADR-0043：持久化落盘后 `persisted = true` 同样合法；缺键才是契约违例。
        assert_eq!(
            validate_settings_result(
                &SettingsResult {
                    rules: valid.clone(),
                    persisted: Some(true),
                },
                true,
            ),
            Ok(())
        );
        assert_eq!(
            validate_settings_result(
                &SettingsResult {
                    rules: valid.clone(),
                    persisted: None,
                },
                true,
            )
            .unwrap_err()
            .field,
            RulesField::Persisted
        );

        let mut invalid = valid.clone();
        invalid.muted_accounts = vec!["work".into(), " ".into()];
        assert_eq!(
            validate_rules_settings(&invalid).unwrap_err().field,
            RulesField::MutedAccount(1)
        );
        let patch = RulesPatch {
            muted_accounts: Some(vec!["a".into(); MAX_MUTED_ACCOUNTS + 1]),
            ..Default::default()
        };
        assert_eq!(
            validate_rules_patch(&patch).unwrap_err().field,
            RulesField::MutedAccounts
        );
    }

    /// ADR-0043：新字段在缺省值时不出现在 JSON 里——发给旧 daemon 的整包与旧客户端逐字节一致；
    /// 旧 daemon 的响应缺这两个键也能解析。
    #[test]
    fn rules_new_mail_fields_are_additive_both_ways() {
        let old_wire = json!({
            "show_sensitive": true,
            "silent_keywords": [],
            "quiet_hours": [],
            "timezone_offset_minutes": 0
        });
        let parsed: RulesSettings = serde_json::from_value(old_wire.clone()).unwrap();
        assert!(!parsed.notify_new_mail);
        assert!(parsed.muted_accounts.is_empty());
        assert_eq!(serde_json::to_value(&parsed).unwrap(), old_wire);
        assert_eq!(serde_json::to_value(RulesPatch::default()).unwrap(), json!({}));

        let patch = RulesPatch {
            notify_new_mail: Some(true),
            muted_accounts: Some(vec!["work".into()]),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&patch).unwrap(),
            json!({ "notify_new_mail": true, "muted_accounts": ["work"] })
        );
    }

    #[test]
    fn status_runtime_metrics_are_optional_and_provider_discovery_parses() {
        let old: StatusResult =
            serde_json::from_value(json!({ "version": "1", "accounts": [], "message_count": 3 }))
                .unwrap();
        assert_eq!(old.runtime, None);
        let new: StatusResult = serde_json::from_value(json!({
            "version": "1", "accounts": [], "message_count": 3,
            "runtime": { "rss_bytes": 10, "phys_footprint_bytes": null, "store_db_bytes": 4096,
                         "store_wal_bytes": 0, "stored_messages": 3, "stored_attachment_bytes": 0 }
        }))
        .unwrap();
        let runtime = new.runtime.unwrap();
        assert_eq!(runtime.rss_bytes, Some(10));
        assert_eq!(runtime.phys_footprint_bytes, None);

        let discovery: ProviderDiscovery = serde_json::from_value(json!({
            "provider_id": "qq", "display_name": "QQ 邮箱", "source": "preset",
            "confidence": "verified",
            "incoming": { "server": "imap.qq.com", "port": 993, "security": "implicit" },
            "mailbox": "INBOX", "idle": true,
            "outgoing": { "server": "smtp.qq.com", "port": 465, "security": "implicit" },
            "username_format": "full_address", "suggested_username": "a@qq.com",
            "auth_kind": "authorization_code", "strip_secret_spaces": false,
            "notes": "", "help_url": null, "future_field": 1
        }))
        .unwrap();
        assert_eq!(discovery.outgoing.port, 465);
    }

    #[test]
    fn notification_status_keeps_unknown_platform_values_forward_compatible() {
        let status: NotificationStatus = serde_json::from_value(json!({
            "provider": "future-native",
            "availability": "future-availability",
            "authorization": "future-authorization",
            "request_in_flight": false,
            "fresh": true
        }))
        .unwrap();
        assert_eq!(status.availability, NotificationAvailability::Unknown);
        assert_eq!(status.authorization, NotificationAuthorization::Unknown);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_rejects_oversized_response_before_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        MAX_FRAME_BYTES + 1
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let client = RpcClient::new(Endpoint::Http {
            url: format!("http://{address}/rpc"),
            token: "t".repeat(32),
        })
        .unwrap();
        let error = client.call("status", json!({})).await.unwrap_err();
        assert!(error.message.contains("响应超过"));
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn typed_status_decodes_http_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request.contains("authorization: Bearer ")
                    || request.contains("Authorization: Bearer ")
            );
            let body = r#"{"jsonrpc":"2.0","result":{"version":"0.1.0","accounts":[],"message_count":7},"id":1}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let client = RpcClient::new(Endpoint::Http {
            url: format!("http://{address}/rpc"),
            token: "t".repeat(32),
        })
        .unwrap();
        let status = client.status().await.unwrap();
        assert_eq!(
            status,
            StatusResult {
                version: "0.1.0".into(),
                accounts: Vec::new(),
                message_count: 7,
                runtime: None,
            }
        );
        server.await.unwrap();
    }

    async fn serve_agent_info_once(response_body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let _ = String::from_utf8_lossy(&request[..read]).to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                        response_body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        format!("http://{address}/rpc")
    }

    #[test]
    fn compat_verdict_follows_protocol_generation_table() {
        let info = |protocol_version: u64| AgentInfo {
            version: "0.1.0".into(),
            protocol_version,
            pid: 1,
            started_at: Some(7),
            build_rev: None,
            executable_path: None,
        };
        assert_eq!(compat_verdict(2, &info(2)), CompatVerdict::Compatible);
        assert_eq!(compat_verdict(2, &info(1)), CompatVerdict::KernelOutdated);
        assert_eq!(compat_verdict(2, &info(3)), CompatVerdict::AppOutdated);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_compat_accepts_matching_generation() {
        let url = serve_agent_info_once(
            r#"{"jsonrpc":"2.0","result":{"version":"0.1.0","protocol_version":1,"pid":42,"started_at":5},"id":1}"#,
        )
        .await;
        let client = RpcClient::new(Endpoint::Http {
            url,
            token: "t".repeat(32),
        })
        .unwrap();
        assert_eq!(
            client.agent_compat(PROTOCOL_VERSION).await.unwrap(),
            CompatVerdict::Compatible
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn method_not_found_means_legacy_kernel() {
        let url = serve_agent_info_once(
            r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"method not found: getAgentInfo"},"id":1}"#,
        )
        .await;
        let client = RpcClient::new(Endpoint::Http {
            url,
            token: "t".repeat(32),
        })
        .unwrap();
        assert_eq!(
            client.agent_compat(PROTOCOL_VERSION).await.unwrap(),
            CompatVerdict::KernelOutdated
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn transport_errors_do_not_produce_verdicts() {
        // 绑定后立即释放端口，连接必然被拒：握手必须上抛而不是给出版本结论。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let client = RpcClient::new(Endpoint::Http {
            url: format!("http://{address}/rpc"),
            token: "t".repeat(32),
        })
        .unwrap();
        let error = client.agent_compat(PROTOCOL_VERSION).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Connection);
    }

    #[test]
    fn open_intent_wire_shape_is_typed_and_forward_compatible() {
        let message = OpenIntent::Message {
            folder: None,
            account: "main".into(),
            uid: "42".into(),
        };
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({ "route": "message", "account": "main", "uid": "42" })
        );
        assert_eq!(
            serde_json::from_value::<OpenIntent>(json!({ "route": "future_route", "x": 1 }))
                .unwrap(),
            OpenIntent::Unknown
        );
    }

    #[test]
    fn frontend_catalog_keeps_lifecycle_and_capabilities_forward_compatible() {
        let catalog: FrontendsResult = serde_json::from_value(json!({
            "default": "org.example.reader",
            "frontends": [{
                "id": "org.example.reader",
                "name": "Reader",
                "version": "2.0.0",
                "lifecycle": "future_lifecycle",
                "capabilities": ["message", "future_route"],
                "builtin": false
            }]
        }))
        .unwrap();
        assert_eq!(catalog.frontends[0].lifecycle, "future_lifecycle");
        assert_eq!(catalog.frontends[0].capabilities[1], "future_route");

        let plugins: ExternalPluginsResult = serde_json::from_value(json!({
            "plugins": [{
                "id": "org.example.worker",
                "name": "Worker",
                "version": "1.0.0",
                "lifecycle": "future_lifecycle",
                "capabilities": ["future_capability"],
                "enabled": true,
                "state": "future_state",
                "restart_count": 3
            }]
        }))
        .unwrap();
        assert_eq!(plugins.plugins[0].state, "future_state");
        assert_eq!(plugins.plugins[0].capabilities[0], "future_capability");
    }

    #[test]
    fn address_view_and_recipient_decode_tolerantly() {
        // ADR-0022：地址视图全字段缺省可解码；消息 DTO 的 recipient 在
        // 旧 daemon 响应里缺席时回填空串。
        let view: AddressView = serde_json::from_value(json!({
            "id": "addr_aaaaaaaaaaaaaaaaaaaaaaaaaa",
            "address": "shop12@example.test",
            "kind": "disposable",
            "state": "active",
        }))
        .unwrap();
        assert_eq!(view.account, "");
        assert_eq!(view.label, "");
        assert_eq!(view.created_at, 0);
        assert_eq!(view.expires_at, None);

        let summary: MessageSummary = serde_json::from_value(json!({
            "account": "acct_x",
            "uid": "5",
            "sender": "s@t",
            "subject": "m",
            "received_at": 7
        }))
        .unwrap();
        assert_eq!(summary.recipient, "");

        let message: MailMessage = serde_json::from_value(json!({
            "account": "acct_x",
            "uid": "6",
            "sender": "s@t",
            "recipient": "shop12@example.test",
            "subject": "m",
            "body": "b",
            "received_at": 7
        }))
        .unwrap();
        assert_eq!(message.recipient, "shop12@example.test");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn rpc_calls_status_over_unix_socket() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("maild-client-{}-{suffix}.sock", std::process::id()));
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = tokio::io::split(stream);
            let mut read = BufReader::new(read);
            let mut line = String::new();
            read.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            write
                .write_all(
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"result\":{{\"message_count\":0}},\"id\":{}}}\n",
                        request["id"]
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let client = RpcClient::new(Endpoint::Unix { path: path.clone() }).unwrap();
        let result = client.call("status", json!({})).await.unwrap();
        assert_eq!(result["message_count"], 0);
        server.await.unwrap();
        std::fs::remove_file(path).unwrap();
    }

    /// ADR-0032：一个按脚本应答 `readAttachment` 的假 daemon。`RpcClient::call` 在 IPC 上每请求
    /// 开一条连接，所以服务端按连接循环 accept；脚本给出每个 offset 对应的应答构造器。
    #[cfg(unix)]
    async fn fake_attachment_server(
        chunks: Vec<Value>,
    ) -> (std::path::PathBuf, tokio::task::JoinHandle<Vec<u64>>) {
        // Clock resolution can repeat under parallel tests; a per-process sequence cannot.
        static NEXT_ATTACHMENT_SOCKET: AtomicU64 = AtomicU64::new(0);
        let suffix = NEXT_ATTACHMENT_SOCKET.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "maild-client-att-{}-{suffix}.sock",
            std::process::id()
        ));
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let mut offsets = Vec::new();
            for chunk in chunks {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = tokio::io::split(stream);
                let mut read = BufReader::new(read);
                let mut line = String::new();
                read.read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], "readAttachment");
                assert_eq!(request["params"]["folder"], "INBOX", "folder 必须随请求发出");
                offsets.push(request["params"]["offset"].as_u64().unwrap());
                let response = json!({ "jsonrpc": "2.0", "result": chunk, "id": request["id"] });
                write
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
            }
            offsets
        });
        (path, server)
    }

    #[cfg(unix)]
    fn chunk(offset: u64, bytes: &[u8], size: u64, eof: bool) -> Value {
        use base64::Engine as _;
        json!({
            "attachment": { "part_index": 2, "filename": "a.bin", "mime_type": "application/octet-stream", "size": size, "stored": true },
            "offset": offset,
            "length": bytes.len(),
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            "eof": eof,
        })
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn read_attachment_reassembles_chunks_by_advancing_offset() {
        let (path, server) = fake_attachment_server(vec![
            chunk(0, b"hello ", 11, false),
            chunk(6, b"world", 11, true),
        ])
        .await;
        let client = RpcClient::new(Endpoint::Unix { path: path.clone() }).unwrap();
        let assembled = client
            .read_attachment("a", "INBOX", "7", 2)
            .await
            .unwrap();
        assert_eq!(assembled.bytes, b"hello world");
        assert_eq!(assembled.attachment.filename, "a.bin");
        assert!(assembled.attachment.stored);
        assert_eq!(server.await.unwrap(), vec![0, 6], "offset 按已收字节推进");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn read_attachment_refuses_servers_that_lie_about_size_or_offsets() {
        // 1. 累计超过宣称的 size：不无界拼装。
        let (path, server) = fake_attachment_server(vec![
            chunk(0, b"hello ", 8, false),
            chunk(6, b"world", 8, true),
        ])
        .await;
        let client = RpcClient::new(Endpoint::Unix { path: path.clone() }).unwrap();
        let error = client.read_attachment("a", "INBOX", "7", 2).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Rpc);
        assert!(error.message.contains("size"), "{}", error.message);
        server.await.unwrap();
        std::fs::remove_file(path).unwrap();

        // 2. size 中途改变：读到的不再是同一个附件。
        let (path, server) = fake_attachment_server(vec![
            chunk(0, b"hello ", 11, false),
            chunk(6, b"world", 12, true),
        ])
        .await;
        let client = RpcClient::new(Endpoint::Unix { path: path.clone() }).unwrap();
        let error = client.read_attachment("a", "INBOX", "7", 2).await.unwrap_err();
        assert!(error.message.contains("变化"), "{}", error.message);
        server.await.unwrap();
        std::fs::remove_file(path).unwrap();

        // 3. eof 早于 size：不把半个文件当完整文件。
        let (path, server) = fake_attachment_server(vec![chunk(0, b"hello", 11, true)]).await;
        let client = RpcClient::new(Endpoint::Unix { path: path.clone() }).unwrap();
        let error = client.read_attachment("a", "INBOX", "7", 2).await.unwrap_err();
        assert!(error.message.contains("eof"), "{}", error.message);
        server.await.unwrap();
        std::fs::remove_file(path).unwrap();

        // 4. offset 与已收不符：服务端回了别的块。
        let (path, server) = fake_attachment_server(vec![chunk(3, b"hello", 11, false)]).await;
        let client = RpcClient::new(Endpoint::Unix { path: path.clone() }).unwrap();
        let error = client.read_attachment("a", "INBOX", "7", 2).await.unwrap_err();
        assert!(error.message.contains("offset"), "{}", error.message);
        server.await.unwrap();
        std::fs::remove_file(path).unwrap();

        // 5. length 与 data 不符。
        let mut lying = chunk(0, b"hello", 5, true);
        lying["length"] = json!(4);
        let (path, server) = fake_attachment_server(vec![lying]).await;
        let client = RpcClient::new(Endpoint::Unix { path: path.clone() }).unwrap();
        let error = client.read_attachment("a", "INBOX", "7", 2).await.unwrap_err();
        assert!(error.message.contains("length"), "{}", error.message);
        server.await.unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn attachment_and_policy_dtos_tolerate_old_and_new_daemons() {
        // 旧 daemon：没有 stored、没有 remote_content。
        let old: MessageResult = serde_json::from_value(json!({
            "message": {
                "account": "a", "uid": "1", "sender": "s", "subject": "x", "body": "", "received_at": 1,
                "attachments": [{ "part_index": 2, "size": 9 }]
            }
        }))
        .unwrap();
        assert!(!old.message.attachments[0].stored, "不确定时不制造下载入口");
        assert!(old.remote_content.is_none());

        // 新 daemon：都有；未知的未来 reason 值不破坏反序列化。
        let new: MessageResult = serde_json::from_value(json!({
            "message": {
                "account": "a", "uid": "1", "sender": "s", "subject": "x", "body": "", "received_at": 1,
                "attachments": [{ "part_index": 2, "size": 9, "stored": true }]
            },
            "remote_content": { "decision": "allow", "reason": "some_future_reason" }
        }))
        .unwrap();
        assert!(new.message.attachments[0].stored);
        assert!(new.remote_content.as_ref().unwrap().allows());

        // patch 只发送给出的字段。
        let patch = ReadingPolicyPatch {
            trust: vec!["a@example.com".into()],
            ..Default::default()
        };
        assert_eq!(serde_json::to_value(&patch).unwrap(), json!({ "trust": ["a@example.com"] }));
        let policy: ReadingPolicyResult = serde_json::from_value(json!({
            "policy": { "remote_content": "block", "trusted_senders": [{ "value": "a@example.com", "kind": "address", "added_at": 5 }] },
            "persisted": true
        }))
        .unwrap();
        assert_eq!(policy.policy.trusted_senders[0].kind, "address");
        assert!(policy.persisted);
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "current_thread")]
    async fn rpc_calls_status_over_named_pipe() {
        use tokio::net::windows::named_pipe::ServerOptions;

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!(r"\\.\pipe\maild-client-{}-{suffix}", std::process::id());
        let server_pipe = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let server = tokio::spawn(async move {
            server_pipe.connect().await.unwrap();
            let (read, mut write) = tokio::io::split(server_pipe);
            let mut read = BufReader::new(read);
            let mut line = String::new();
            read.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            write
                .write_all(
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"result\":{{\"message_count\":0}},\"id\":{}}}\n",
                        request["id"]
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let client = RpcClient::new(Endpoint::NamedPipe { name }).unwrap();
        let result = client.call("status", json!({})).await.unwrap();
        assert_eq!(result["message_count"], 0);
        server.await.unwrap();
    }
}
