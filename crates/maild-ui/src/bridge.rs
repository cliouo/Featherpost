//! 壳与 daemon 之间唯一的通道（ADR-0031 决策 4）。
//!
//! `Bridge` 包着 `maild-app` 的连接 actor，以 `frontend:<FRONTEND_ID>` 主体运行（ADR-0030 决策 6
//! 的 bootstrap 住在 actor 里）。它对外只给投影后的 DTO 与四类错误码；socket 路径、原始异常、
//! `body_html` 都不出这个模块——前两者进 stderr，后者等 M4.5-04 的隔离文档。
//!
//! 这个模块不依赖 Tauri，能在没有窗口的 hermetic e2e 里对着真 daemon 跑。

use std::collections::HashMap;
use std::time::Duration;

use maild_app::{
    ActorOptions, CompatVerdict, ConnectionHandle, ConnectionPhase, FrontendIdentity,
    IdentityFailure, ViewState,
};
use maild_client::{
    assemble_attachment, AttachmentBytes, AttachmentChunk, ClientError, ContactSuggestion,
    Endpoint, ErrorKind, FrontendsResult, LocalePreferenceResult, LocalePreferenceUpdate,
    MailAttachment, MailFolder, MailMessage, MessageResult, MessageSummariesResult, MessageSummary,
    OpenIntent, PackageL10n, ReadingPolicyPatch, ReadingPolicyResult, SearchContactsResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::documents::{Document, DocumentIdentity, DocumentStore, ProtocolBase};
use crate::html::{self, LinkKind};

/// 本前端在受管目录与 manifest 里的 id（`package/frontend.toml`），来自 `brand.toml`。
pub const FRONTEND_ID: &str = maild_brand::BUNDLED_FRONTEND_ID;
/// 连接快照事件；载荷是 [`UiSnapshot`]。
pub const STATE_EVENT: &str = "maild://state";
/// 导航意图事件；载荷是 `OpenIntent`（ADR-0014 seam 的消费侧）。
pub const INTENT_EVENT: &str = "maild://intent";
/// 邮件里的链接被点击；载荷是 [`UiLink`]（ADR-0033 决策 6）。
pub const LINK_EVENT: &str = "maild://link";
/// 语言偏好变了；载荷是 [`UiLocale`]（ADR-0048）。
pub const LOCALE_EVENT: &str = "maild://locale";
/// 全局代理偏好或系统代理变了；载荷是 [`crate::settings::UiProxyEvent`]（不含任何密码）。
pub const PROXY_EVENT: &str = "maild://proxy";

const MAX_PAGE: u64 = 200;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiErrorCode {
    /// 连接层不可用；actor 在退避重连，UI 只读缓存。
    Offline,
    /// `-32016`：本前端没有该方法的授权（ADR-0030）。
    Denied,
    /// `-32002`：对象不在本地缓存里。
    NotFound,
    /// 参数无效（本地校验或 `-32602`）。
    Invalid,
    /// 其他失败；原因在 stderr。
    Failed,
    Conflict,
    Limit,
    Locked,
    /// ADR-0016 握手判定版本不一致：写入在桥内被挡下，请求没有发给后台。
    Incompatible,
}

/// ADR-0016 握手结论的 UI 投影。只依据 `protocol_version`；每个连接 epoch 判一次。
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiCompat {
    /// 尚未握手，或握手因连接问题没有结论。不阻止任何请求。
    #[default]
    Unknown,
    Compatible,
    /// 后台协议代较旧（或是早于握手的遗留后台）。
    KernelOutdated,
    /// 本界面协议代较旧。
    AppOutdated,
}

impl From<CompatVerdict> for UiCompat {
    fn from(verdict: CompatVerdict) -> Self {
        match verdict {
            CompatVerdict::Compatible => Self::Compatible,
            CompatVerdict::KernelOutdated => Self::KernelOutdated,
            CompatVerdict::AppOutdated => Self::AppOutdated,
        }
    }
}

impl UiCompat {
    /// 版本不一致时只放行只读请求；写入、探测、安装、发送一律不发出。
    pub fn blocks(self, method: &str) -> bool {
        matches!(self, Self::KernelOutdated | Self::AppOutdated) && !is_read_only(method)
    }

    fn denial(self) -> UiError {
        let product = maild_brand::PRODUCT_NAME;
        let (reason, message) = match self {
            Self::KernelOutdated => ("kernel_outdated", format!(
                "后台版本较旧，与当前界面不兼容，写入已暂停。请重新启动后台；若刚回退过 {product}，请从菜单栏「修复与后台管理…」重装内置界面。"
            )),
            _ => ("app_outdated", format!(
                "当前界面版本较旧，与后台不兼容，写入已暂停。请从菜单栏「修复与后台管理…」重装内置界面，或更新 {product}。"
            )),
        };
        UiError {
            code: UiErrorCode::Incompatible,
            reason,
            message,
        }
    }
}

/// 只读方法按名字前缀判定，外加一张显式白名单：新增方法默认视为写入（版本不一致时 fail-closed）。
///
/// 白名单里的方法不改服务器、账户或设置状态：`syncMessages` 只是提前一次本来就会发生的增量
/// 同步，`discoverEmailProvider`/`listEmailPresets` 只读内置知识库（可选有界探测）。
/// `fetchServerMessage` 会把一封信落进本地库，故意不列入——版本不一致时与其他写入一样挡下。
fn is_read_only(method: &str) -> bool {
    const EXPLICIT_READS: [&str; 4] = [
        "whoami",
        "syncMessages",
        "discoverEmailProvider",
        "listEmailPresets",
    ];
    ["get", "list", "search"]
        .iter()
        .any(|prefix| method.starts_with(prefix))
        || EXPLICIT_READS.contains(&method)
}

#[derive(Default)]
struct CompatRecord {
    verdict: UiCompat,
    checked_epoch: Option<u64>,
    in_flight: bool,
}

/// webview 能看到的全部错误信息。`message` 是固定的中文文案，不是 daemon 原文。
///
/// ADR-0048 决策 3：`reason` 是稳定的原因码（snake_case），webview 据此按当前界面语言成文
/// （`packages/featherpost-studio/src/bridge/errors.ts`）；`message` 只作诊断与兜底，不是协议契约。
/// 新增 reason 时同步在 webview 的映射表与两种语言的消息里登记，前端测试会核对。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiError {
    pub code: UiErrorCode,
    pub reason: &'static str,
    pub message: String,
}

impl UiError {
    pub(crate) fn invalid(reason: &'static str, message: &str) -> Self {
        Self {
            code: UiErrorCode::Invalid,
            reason,
            message: message.into(),
        }
    }

    pub(crate) fn not_found(reason: &'static str, message: &str) -> Self {
        Self {
            code: UiErrorCode::NotFound,
            reason,
            message: message.into(),
        }
    }

    pub(crate) fn failed(
        context: &str,
        detail: impl std::fmt::Display,
        reason: &'static str,
        message: &str,
    ) -> Self {
        eprintln!("[maild-ui] {context} 失败: {detail}");
        Self {
            code: UiErrorCode::Failed,
            reason,
            message: message.into(),
        }
    }

    /// 投影：原文只进 stderr。socket 路径、SQLite 文本、反序列化细节都不是用户该看的东西，
    /// 也不该被扩展视图（E02）间接读到。
    pub(crate) fn from_client(context: &str, error: ClientError) -> Self {
        eprintln!("[maild-ui] {context} 失败: {error}");
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
            (_, Some(-32002)) => (
                UiErrorCode::NotFound,
                "message_not_cached",
                "这封邮件已不在本地缓存中。",
            ),
            // ADR-0032 决策 3：两种「拿不到附件」对用户是不同的话。
            (_, Some(-32003)) => (
                UiErrorCode::NotFound,
                "attachment_not_in_message",
                "这个附件不在这封邮件里。",
            ),
            (_, Some(-32004)) => (
                UiErrorCode::NotFound,
                "attachment_not_stored",
                "这个附件的字节没有保存在本地缓存里。",
            ),
            (_, Some(-32005)) => (UiErrorCode::NotFound, "draft_missing", "这份草稿已不存在。"),
            (_, Some(-32006)) => (
                UiErrorCode::Conflict,
                "draft_conflict",
                "草稿已在别处修改。你的输入仍保留，请复制内容或重新载入。",
            ),
            (_, Some(-32007)) => (
                UiErrorCode::Limit,
                "draft_too_large",
                "内容或附件超过保存上限，请减少后再试。",
            ),
            (_, Some(-32008)) => (
                UiErrorCode::NotFound,
                "upload_expired",
                "附件上传已过期，请重新选择文件。",
            ),
            (_, Some(-32021)) => (
                UiErrorCode::Locked,
                "draft_locked",
                "这份草稿正在发送、已发送或结果尚未确认。",
            ),
            (_, Some(-32602)) => (UiErrorCode::Invalid, "invalid_params", "请求参数无效。"),
            _ => (UiErrorCode::Failed, "failed", "操作未完成，请稍后重试。"),
        };
        Self {
            code,
            reason,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiAccount {
    pub account: String,
    pub state: String,
    pub detail: String,
    /// 离线原因的稳定 code（ADR-0048 L6）；webview 按它用界面语言成文，`detail` 的中文只作兜底。
    /// 老 daemon 没有。
    pub detail_code: Option<String>,
    /// 服务器或系统原文（不含 daemon 的前缀），可空；webview 作为次要信息原样显示。
    pub server_detail: String,
}

/// 连接快照。故意没有 `last_error` 字段：那串文本可能带 socket 路径，UI 只需要知道
/// 「现在能不能写」与「身份有没有问题」两件事，其余看 stderr。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiSnapshot {
    pub frontend_id: String,
    /// `disconnected` / `connecting` / `synchronizing` / `online` / `retry_waiting` / `stopped`
    pub phase: &'static str,
    pub attempt: Option<u32>,
    pub retry_in_ms: Option<u64>,
    pub revision: u64,
    pub connection_epoch: u64,
    pub version: Option<String>,
    pub accounts: Vec<UiAccount>,
    /// `whoami` 回报的主体；未绑定为 `None`。
    pub principal: Option<String>,
    /// 身份 bootstrap 失败原因（我们自己拼的文案，不含路径）。修复动作是安装前端包。
    pub identity_failure: Option<String>,
    /// 失败的稳定原因（`not_installed` / `rejected` / `principal_mismatch` / `unavailable`，
    /// ADR-0048 决策 3）。webview 按它用界面语言成文，上面的中文只作诊断。
    pub identity_failure_code: Option<&'static str>,
    /// 快照有部分失败（actor 的 `partial_snapshot`）。
    pub partial: bool,
    /// 最近一次版本握手结论（ADR-0016）。重连后在新结论出来前沿用上一次的。
    pub compat: UiCompat,
}

/// 纯函数投影，便于单测「不泄漏」性质。
pub fn project(state: &ViewState, frontend_id: &str, compat: UiCompat) -> UiSnapshot {
    let (phase, attempt, retry_in_ms) = match &state.phase {
        ConnectionPhase::Disconnected => ("disconnected", None, None),
        ConnectionPhase::Connecting { attempt } => ("connecting", Some(*attempt), None),
        ConnectionPhase::Synchronizing => ("synchronizing", None, None),
        ConnectionPhase::Online => ("online", None, None),
        ConnectionPhase::RetryWaiting { attempt, delay } => (
            "retry_waiting",
            Some(*attempt),
            Some(u64::try_from(delay.as_millis()).unwrap_or(u64::MAX)),
        ),
        ConnectionPhase::Stopped => ("stopped", None, None),
    };
    UiSnapshot {
        frontend_id: frontend_id.into(),
        phase,
        attempt,
        retry_in_ms,
        revision: state.revision,
        connection_epoch: state.connection_epoch,
        version: state.version.clone(),
        accounts: state
            .accounts
            .iter()
            .map(|account| UiAccount {
                account: account.account.clone(),
                state: account.state.clone(),
                detail: account.detail.clone(),
                detail_code: account.detail_code.clone(),
                server_detail: account.server_detail.clone(),
            })
            .collect(),
        principal: state.principal.clone(),
        identity_failure: state.identity_failure.clone(),
        identity_failure_code: state.identity_failure_code.map(IdentityFailure::code),
        partial: state.partial_snapshot,
        compat,
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiFolder {
    pub account: String,
    /// 线上原名，作为后续请求的 `folder` 参数；展示用 `display_name`。
    pub name: String,
    pub display_name: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_primary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_search_inbox: Option<bool>,
    pub present: bool,
    pub total: i64,
    pub unseen: i64,
}

impl From<MailFolder> for UiFolder {
    fn from(folder: MailFolder) -> Self {
        Self {
            account: folder.account,
            display_name: if folder.display_name.is_empty() {
                folder.name.clone()
            } else {
                folder.display_name
            },
            name: folder.name,
            role: folder.role,
            is_primary: folder.is_primary,
            is_search_inbox: folder.is_search_inbox,
            present: folder.present,
            total: folder.total,
            unseen: folder.unseen,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiSummary {
    pub account: String,
    pub folder: String,
    pub uid: String,
    pub sender: String,
    pub recipient: String,
    pub subject: String,
    pub snippet: String,
    pub received_at: u64,
    /// 发件人声明的时间（可能缺失、可能不可信）；排序用 `sort_at`（ADR-0025）。
    pub date_at: Option<u64>,
    pub sort_at: u64,
    pub has_attachments: bool,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub deleted: bool,
    pub thread_id: i64,
}

impl From<MessageSummary> for UiSummary {
    fn from(summary: MessageSummary) -> Self {
        Self {
            account: summary.account,
            folder: summary.folder,
            uid: summary.uid,
            sender: summary.sender,
            recipient: summary.recipient,
            subject: summary.subject,
            snippet: summary.snippet,
            received_at: summary.received_at,
            date_at: summary.date_at,
            sort_at: summary.sort_at,
            has_attachments: summary.has_attachments,
            seen: summary.seen,
            flagged: summary.flagged,
            answered: summary.answered,
            deleted: summary.deleted,
            thread_id: summary.thread_id,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiPage {
    pub messages: Vec<UiSummary>,
    pub total: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiContactSuggestion {
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl From<ContactSuggestion> for UiContactSuggestion {
    fn from(suggestion: ContactSuggestion) -> Self {
        Self {
            address: suggestion.address,
            name: suggestion.name,
        }
    }
}

/// `searchContacts` 错误投影：清晰区分后台离线、旧版后台不支持（`-32601` 或旧后台对未知方法
/// fail-closed 返回的 `仅限用户主体` `-32016`）与真实缺 `messages_read` 授权（`-32016`）。
fn search_contacts_error(error: ClientError) -> UiError {
    let unsupported = error.rpc_code == Some(-32601)
        || (error.rpc_code == Some(-32016) && error.message.contains("仅限用户主体"));
    if unsupported {
        eprintln!("[maild-ui] searchContacts 失败（后台不支持联系人检索）: {error}");
        return UiError {
            code: UiErrorCode::Failed,
            reason: "contacts_unsupported",
            message: "后台版本不支持联系人自动补全。".into(),
        };
    }
    UiError::from_client("searchContacts", error)
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiAttachment {
    pub part_index: u32,
    pub filename: String,
    pub mime_type: String,
    pub content_id: String,
    pub inline: bool,
    pub size: u64,
    /// 字节已落库、可保存（ADR-0032 决策 3）。为假时 UI 写「尚未保存此附件」，不给按钮。
    pub stored: bool,
}

impl From<MailAttachment> for UiAttachment {
    fn from(attachment: MailAttachment) -> Self {
        Self {
            part_index: attachment.part_index,
            filename: attachment.filename,
            mime_type: attachment.mime_type,
            content_id: attachment.content_id,
            inline: attachment.inline,
            size: attachment.size,
            stored: attachment.stored,
        }
    }
}

/// 此刻对这封信远程内容的内核结论（ADR-0032 决策 5）。`decision` 取 `block`/`allow`，
/// `reason` 取 `default`/`trusted_address`/`trusted_domain`/`allow_all`。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiRemoteContent {
    pub decision: String,
    pub reason: String,
}

impl UiRemoteContent {
    /// 旧 daemon 没有这个键时的结论：与缺省策略一致。
    fn fallback() -> Self {
        Self {
            decision: "block".into(),
            reason: "default".into(),
        }
    }
}

/// 详情。**没有 `body_html`**：HTML 只进 M4.5-04 的隔离文档（`ui_prepare_html`），不进宿主 webview。
/// 这里只告诉 UI「有 HTML 版式」与体积、附件元数据与远程内容结论。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiMessage {
    #[serde(flatten)]
    pub summary: UiSummary,
    pub cc: String,
    pub reply_to: String,
    pub body: String,
    pub has_html: bool,
    pub html_bytes: usize,
    pub attachments: Vec<UiAttachment>,
    pub remote_content: UiRemoteContent,
}

/// `ui_prepare_html` 的结果（ADR-0033 决策 4/5/7）。`token`/`url` 为 `None` = 这封信没有 HTML。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiDocument {
    pub token: Option<String>,
    pub url: Option<String>,
    pub has_html: bool,
    pub truncated: bool,
    pub remote_refs: usize,
    pub cid_resolved: usize,
    pub cid_missing: usize,
    pub dropped_refs: usize,
    pub allow_remote: bool,
    pub links: usize,
}

/// 邮件里被点击的链接（事件 [`LINK_EVENT`] 的载荷）。`kind` 是 `http`/`https`/`mailto`/`other`。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiLink {
    pub token: String,
    pub index: usize,
    pub href: String,
    pub kind: LinkKind,
    pub host: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiTrustedSender {
    pub value: String,
    pub kind: String,
    pub added_at: u64,
}

/// 阅读策略（ADR-0032 决策 5）。`persisted = false` 表示策略文件停摆：读到缺省、写会被拒。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiReadingPolicy {
    pub remote_content: String,
    pub trusted_senders: Vec<UiTrustedSender>,
    pub persisted: bool,
}

impl From<ReadingPolicyResult> for UiReadingPolicy {
    fn from(result: ReadingPolicyResult) -> Self {
        Self {
            remote_content: result.policy.remote_content,
            trusted_senders: result
                .policy
                .trusted_senders
                .into_iter()
                .map(|entry| UiTrustedSender {
                    value: entry.value,
                    kind: entry.kind,
                    added_at: entry.added_at,
                })
                .collect(),
            persisted: result.persisted,
        }
    }
}

/// webview 侧提交的策略 patch；校验在 daemon（原子、整包拒绝）。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct UiReadingPolicyPatch {
    #[serde(default)]
    pub remote_content: Option<String>,
    #[serde(default)]
    pub trust: Vec<String>,
    #[serde(default)]
    pub untrust: Vec<String>,
}

/// ADR-0048：[`LOCALE_EVENT`] 的载荷与 `ui_locale` 的结果。`effective` 是壳的有序语言列表：显式模式下
/// 即 daemon 的 `effective`，跟随系统时是本进程的首选语言（M2，见 `shell_locale`）；
/// `None` = 尚未取到或 daemon 不提供，webview 回落基础语言。协商由 webview 自己做。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiLocale {
    pub effective: Option<Vec<String>>,
}

/// 语言偏好（`getLocalePreference` / `setLocalePreference` 的投影）。`mode` 取 `system` / `explicit`；
/// `persisted = false` 表示偏好文件停摆：读到的是缺省偏好，写会被拒绝。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiLocalePreference {
    pub mode: String,
    pub languages: Vec<String>,
    pub effective: Vec<String>,
    pub persisted: bool,
}

impl From<LocalePreferenceResult> for UiLocalePreference {
    fn from(result: LocalePreferenceResult) -> Self {
        Self {
            mode: result.preference.mode,
            languages: result.preference.languages,
            effective: result.effective,
            persisted: result.persisted,
        }
    }
}

/// webview 提交的偏好：`{mode: "system"}` 或 `{mode: "explicit", languages: [...]}`。
/// 标签校验与规范化在 daemon（整包拒绝、不落盘）。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiLocaleUpdate {
    System,
    Explicit { languages: Vec<String> },
}

/// 语言偏好方法的错误投影。老 daemon 不认识这两个方法：`-32601`，或其授权表对前端主体
/// fail-closed 的 `-32016`（`getLocalePreference` 在新 daemon 上是 `Open`，所以读时的 `-32016`
/// 只可能来自老 daemon；写时的 `-32016` 仍是缺授权）。`-32001` 是偏好文件停摆、写入被拒。
fn locale_error(method: &str, error: ClientError) -> UiError {
    let unsupported = match error.rpc_code {
        Some(-32601) => true,
        Some(-32016) => method == "getLocalePreference",
        _ => false,
    };
    if unsupported {
        eprintln!("[maild-ui] {method} 失败（后台不支持语言偏好）: {error}");
        return UiError {
            code: UiErrorCode::Failed,
            reason: "locale_unsupported",
            message: "后台版本不支持语言偏好。".into(),
        };
    }
    if error.rpc_code == Some(-32001) {
        eprintln!("[maild-ui] {method} 失败: {error}");
        return UiError {
            code: UiErrorCode::Failed,
            reason: "locale_not_persisted",
            message: "语言偏好文件无法写入，这次更改没有保存。".into(),
        };
    }
    UiError::from_client(method, error)
}

impl From<MailMessage> for UiMessage {
    fn from(message: MailMessage) -> Self {
        let summary = UiSummary {
            account: message.account,
            folder: message.folder,
            uid: message.uid,
            sender: message.sender,
            recipient: message.recipient,
            subject: message.subject,
            snippet: message.snippet,
            received_at: message.received_at,
            date_at: message.date_at,
            sort_at: message.sort_at,
            has_attachments: message.has_attachments,
            seen: message.seen,
            flagged: message.flagged,
            answered: message.answered,
            deleted: message.deleted,
            thread_id: message.thread_id,
        };
        Self {
            summary,
            cc: message.cc,
            reply_to: message.reply_to,
            body: message.body,
            has_html: !message.body_html.is_empty(),
            html_bytes: message.body_html.len(),
            attachments: message
                .attachments
                .into_iter()
                .map(UiAttachment::from)
                .collect(),
            remote_content: UiRemoteContent::fallback(),
        }
    }
}

impl From<MessageResult> for UiMessage {
    fn from(result: MessageResult) -> Self {
        let mut message = UiMessage::from(result.message);
        if let Some(decision) = result.remote_content {
            message.remote_content = UiRemoteContent {
                decision: decision.decision,
                reason: decision.reason,
            };
        }
        message
    }
}

/// 本前端在 daemon 眼里的样子：设置页据此显示「已授权的功能」，第三方前端也是同一张表。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UiFrontendInfo {
    pub id: String,
    pub name: String,
    /// ADR-0048 决策 5：manifest 的兜底说明与 `[l10n.<tag>]` 原样透传，webview 渲染时协商。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "PackageL10n::is_empty")]
    pub l10n: PackageL10n,
    pub version: String,
    pub lifecycle: String,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
    pub is_default: bool,
    pub default_frontend: String,
}

pub struct Bridge {
    handle: ConnectionHandle,
    frontend_id: String,
    compat: std::sync::Mutex<CompatRecord>,
    /// 握手结论变化的信号。快照只由状态泵一个任务推送，保证 webview 看到的顺序单调。
    compat_changed: tokio::sync::Notify,
}

impl Bridge {
    /// 以 `frontend:<FRONTEND_ID>` 主体起连接 actor。HTTP 端点在这里就被拒绝
    /// （`maild-app` 校验）：一个声称要以前端主体运行的壳不该静默降级成 `user`。
    pub fn spawn(endpoint: Endpoint) -> Result<Self, ClientError> {
        Self::spawn_with_options(endpoint, ActorOptions::default())
    }

    pub fn spawn_with_options(
        endpoint: Endpoint,
        options: ActorOptions,
    ) -> Result<Self, ClientError> {
        let options = ActorOptions {
            identity: Some(FrontendIdentity {
                frontend_id: FRONTEND_ID.into(),
            }),
            ..options
        };
        let handle = ConnectionHandle::spawn(endpoint, options)?;
        Ok(Self {
            handle,
            frontend_id: FRONTEND_ID.into(),
            compat: std::sync::Mutex::new(CompatRecord::default()),
            compat_changed: tokio::sync::Notify::new(),
        })
    }

    /// 诊断与测试用；壳的命令层不用它。
    pub fn handle(&self) -> &ConnectionHandle {
        &self.handle
    }

    pub fn frontend_id(&self) -> &str {
        &self.frontend_id
    }

    pub fn snapshot(&self) -> UiSnapshot {
        project(&self.handle.state(), &self.frontend_id, self.compat())
    }

    pub fn project(&self, state: &ViewState) -> UiSnapshot {
        project(state, &self.frontend_id, self.compat())
    }

    fn compat_record(&self) -> std::sync::MutexGuard<'_, CompatRecord> {
        self.compat
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn compat(&self) -> UiCompat {
        self.compat_record().verdict
    }

    /// ADR-0016 握手：在线且该 epoch 尚未判定时问一次 `getAgentInfo`。连接类失败不落结论但
    /// 记下当前在线 epoch，等下次重连再试；若握手途中发生断连或切入新 epoch，旧代结果直接丢弃，
    /// 新 epoch 已在线则紧接着补握。返回 true 表示结论变了（同时唤醒 [`Self::compat_changed`]）。
    pub async fn check_compat(&self) -> bool {
        self.check_compat_against(maild_client::PROTOCOL_VERSION)
            .await
    }

    pub async fn check_compat_against(&self, required_protocol_version: u64) -> bool {
        let mut any_changed = false;
        loop {
            let epoch = {
                let state = self.handle.state();
                if !matches!(state.phase, ConnectionPhase::Online) {
                    return any_changed;
                }
                state.connection_epoch
            };
            {
                let mut record = self.compat_record();
                if record.in_flight || record.checked_epoch == Some(epoch) {
                    return any_changed;
                }
                record.in_flight = true;
            }
            let result = self.handle.agent_compat(required_protocol_version).await;
            let current_online_epoch = {
                let state = self.handle.state();
                matches!(state.phase, ConnectionPhase::Online).then_some(state.connection_epoch)
            };
            let mut record = self.compat_record();
            record.in_flight = false;
            if current_online_epoch != Some(epoch) {
                if current_online_epoch.is_some() {
                    continue;
                }
                return any_changed;
            }
            record.checked_epoch = Some(epoch);
            match result {
                Ok(verdict) => {
                    let verdict = UiCompat::from(verdict);
                    if verdict != UiCompat::Compatible {
                        eprintln!("[maild-ui] 版本握手不一致: {verdict:?}；写入已暂停");
                    }
                    if record.verdict != verdict {
                        record.verdict = verdict;
                        self.compat_changed.notify_one();
                        any_changed = true;
                    }
                }
                Err(error) => {
                    eprintln!("[maild-ui] 版本握手失败: {error}");
                }
            }
            return any_changed;
        }
    }

    /// 等到握手结论变化（状态泵据此补推一帧快照）。
    pub async fn compat_changed(&self) {
        self.compat_changed.notified().await;
    }

    /// 所有发往后台的请求在这里过一次版本闸门（命令层的 `compose_call`/`management_call`
    /// 与桥自己的写方法）。
    pub(crate) fn guard(&self, method: &str) -> Result<(), UiError> {
        let compat = self.compat();
        if compat.blocks(method) {
            eprintln!("[maild-ui] 版本不一致，拒绝发出 {method}");
            return Err(compat.denial());
        }
        Ok(())
    }

    pub fn subscribe(&self) -> watch::Receiver<ViewState> {
        self.handle.subscribe_state()
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<maild_app::AppEvent> {
        self.handle.subscribe_events()
    }

    pub async fn list_folders(&self, account: &str) -> Result<Vec<UiFolder>, UiError> {
        if account.is_empty() {
            return Err(UiError::invalid("account_required", "account 不能为空"));
        }
        let result: maild_client::FoldersResult = self
            .handle
            .request_typed("listFolders", json!({ "account": account }))
            .await
            .map_err(|error| UiError::from_client("listFolders", error))?;
        Ok(result.folders.into_iter().map(UiFolder::from).collect())
    }

    pub async fn list_messages(
        &self,
        account: &str,
        folder: &str,
        offset: u64,
        limit: u64,
    ) -> Result<UiPage, UiError> {
        if account.is_empty() || folder.is_empty() {
            return Err(UiError::invalid("folder_required", "account 与 folder 不能为空"));
        }
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(UiError::invalid("page_limit", "limit 必须是 1 到 200"));
        }
        let result: MessageSummariesResult = self
            .handle
            .request_typed(
                "listMessageSummaries",
                json!({ "account": account, "folder": folder, "limit": limit, "offset": offset }),
            )
            .await
            .map_err(|error| UiError::from_client("listMessageSummaries", error))?;
        Ok(UiPage {
            messages: result.messages.into_iter().map(UiSummary::from).collect(),
            total: result.total,
        })
    }

    async fn message_result(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
    ) -> Result<MessageResult, UiError> {
        if account.is_empty() || folder.is_empty() || uid.is_empty() {
            return Err(UiError::invalid("message_identity_required", "account、folder、uid 不能为空"));
        }
        self.handle
            .request_typed(
                "getMessage",
                json!({ "account": account, "folder": folder, "uid": uid }),
            )
            .await
            .map_err(|error| UiError::from_client("getMessage", error))
    }

    pub async fn get_message(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
    ) -> Result<UiMessage, UiError> {
        Ok(UiMessage::from(
            self.message_result(account, folder, uid).await?,
        ))
    }

    pub async fn search_contacts(
        &self,
        account: &str,
        query: Option<&str>,
        limit: Option<u64>,
    ) -> Result<Vec<UiContactSuggestion>, UiError> {
        if account.is_empty() || account.len() > 256 || account.chars().any(char::is_control) {
            return Err(UiError::invalid("account_required", "account 不能为空"));
        }
        if let Some(q) = query {
            if q.len() > 256 || q.chars().any(char::is_control) {
                return Err(UiError::invalid("invalid_params", "请求参数无效。"));
            }
        }
        let mut params = json!({ "account": account });
        if let Some(q) = query {
            params["query"] = json!(q);
        }
        if let Some(l) = limit {
            if l == 0 {
                return Err(UiError::invalid("invalid_params", "请求参数无效。"));
            }
            params["limit"] = json!(l.min(20));
        }
        let result: SearchContactsResult = self
            .handle
            .request_typed("searchContacts", params)
            .await
            .map_err(search_contacts_error)?;
        Ok(result
            .contacts
            .into_iter()
            .map(UiContactSuggestion::from)
            .collect())
    }

    /// 经认证连接分块取回附件字节（ADR-0032 决策 1），循环与护栏与 `RpcClient` 共用。
    pub async fn read_attachment(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
        part_index: u32,
    ) -> Result<AttachmentBytes, UiError> {
        if account.is_empty() || folder.is_empty() || uid.is_empty() {
            return Err(UiError::invalid("message_identity_required", "account、folder、uid 不能为空"));
        }
        assemble_attachment(|offset| async move {
            self.handle
                .request_typed::<AttachmentChunk>(
                    "readAttachment",
                    json!({
                        "account": account, "folder": folder, "uid": uid,
                        "part_index": part_index, "offset": offset,
                    }),
                )
                .await
        })
        .await
        .map_err(|error| UiError::from_client("readAttachment", error))
    }

    /// 准备一封信的隔离文档（ADR-0033 决策 3/4）：取 HTML → 重写 → 以令牌存进 `store`。
    /// 没有 HTML 的信返回 `token = None`，UI 显示纯文本。
    pub async fn prepare_document(
        &self,
        store: &DocumentStore,
        base: &ProtocolBase,
        account: &str,
        folder: &str,
        uid: &str,
        allow_remote: bool,
    ) -> Result<UiDocument, UiError> {
        let result = self.message_result(account, folder, uid).await?;
        let message = result.message;
        if message.body_html.is_empty() {
            return Ok(UiDocument {
                token: None,
                url: None,
                has_html: false,
                truncated: false,
                remote_refs: 0,
                cid_resolved: 0,
                cid_missing: 0,
                dropped_refs: 0,
                allow_remote,
                links: 0,
            });
        }
        let cid_parts: HashMap<String, u32> = message
            .attachments
            .iter()
            .filter(|attachment| attachment.stored && !attachment.content_id.is_empty())
            .map(|attachment| (attachment.content_id.clone(), attachment.part_index))
            .collect();
        let parts: HashMap<u32, String> = message
            .attachments
            .iter()
            .filter(|attachment| attachment.stored)
            .map(|attachment| (attachment.part_index, attachment.mime_type.clone()))
            .collect();
        let token = crate::documents::new_token();
        let prepared = html::prepare(html::PrepareInput {
            html: &message.body_html,
            token: &token,
            part_url_prefix: &base.url_prefix,
            csp_source: &base.csp_source,
            cid_parts: &cid_parts,
            allow_remote,
        })
        .map_err(|error| {
            UiError::failed(
                "prepare_document",
                error,
                "html_unavailable",
                "无法准备 HTML 版式，已显示纯文本。",
            )
        })?;
        let document = UiDocument {
            url: Some(base.doc_url(&token)),
            token: Some(token.clone()),
            has_html: true,
            truncated: prepared.truncated,
            remote_refs: prepared.remote_refs,
            cid_resolved: prepared.cid_resolved,
            cid_missing: prepared.cid_missing,
            dropped_refs: prepared.dropped_refs,
            allow_remote,
            links: prepared.links.len(),
        };
        store.insert(
            token,
            Document {
                identity: DocumentIdentity {
                    account: message.account,
                    folder: message.folder,
                    uid: message.uid,
                },
                subject: message.subject,
                html: prepared.html,
                csp: prepared.csp,
                links: prepared.links,
                parts,
                remote_urls: prepared.remote_urls,
                allow_remote,
                translation: std::sync::Arc::new(std::sync::RwLock::new(None)),
                translation_rev: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
                active_job: std::sync::Arc::new(std::sync::Mutex::new(None)),
            },
        );
        Ok(document)
    }

    /// part 通道（决策 4）：只认令牌，只给该令牌所绑那封信里已落库的 part。
    pub async fn serve_part(
        &self,
        store: &DocumentStore,
        token: &str,
        part_index: u32,
    ) -> Result<(String, Vec<u8>), UiError> {
        let document = store
            .get(token)
            .ok_or_else(|| UiError::not_found("document_released", "文档已释放或不存在。"))?;
        let mime = document
            .parts
            .get(&part_index)
            .cloned()
            .ok_or_else(|| UiError::not_found("part_not_in_document", "这个附件不属于当前文档。"))?;
        let identity = &document.identity;
        let bytes = self
            .read_attachment(
                &identity.account,
                &identity.folder,
                &identity.uid,
                part_index,
            )
            .await?
            .bytes;
        Ok((mime, bytes))
    }

    pub async fn reading_policy(&self) -> Result<UiReadingPolicy, UiError> {
        let result: ReadingPolicyResult = self
            .handle
            .request_typed("getReadingPolicy", json!({}))
            .await
            .map_err(|error| UiError::from_client("getReadingPolicy", error))?;
        Ok(UiReadingPolicy::from(result))
    }

    pub async fn set_reading_policy(
        &self,
        patch: &UiReadingPolicyPatch,
    ) -> Result<UiReadingPolicy, UiError> {
        let params = serde_json::to_value(ReadingPolicyPatch {
            remote_content: patch.remote_content.clone(),
            trust: patch.trust.clone(),
            untrust: patch.untrust.clone(),
        })
        .map_err(|error| {
            UiError::failed("set_reading_policy", error, "failed", "操作未完成，请稍后重试。")
        })?;
        self.guard("setReadingPolicy")?;
        let result: ReadingPolicyResult = self
            .handle
            .request_typed("setReadingPolicy", params)
            .await
            .map_err(|error| UiError::from_client("setReadingPolicy", error))?;
        Ok(UiReadingPolicy::from(result))
    }

    /// ADR-0048：actor 手里当前 daemon 的 `effective` 列表（连接快照或 `preferences/locale` 推送）。
    /// 不发请求，离线也能读。webview 拿到的是壳据此再解析的列表（M2，`shell_locale`）。
    pub fn locale(&self) -> UiLocale {
        UiLocale {
            effective: self.handle.state().locale,
        }
    }

    /// 设置页显示的完整偏好（模式与显式列表）。老 daemon 投影成 `locale_unsupported`。
    pub async fn locale_preference(&self) -> Result<UiLocalePreference, UiError> {
        let result: LocalePreferenceResult = self
            .handle
            .request_typed("getLocalePreference", json!({}))
            .await
            .map_err(|error| locale_error("getLocalePreference", error))?;
        Ok(UiLocalePreference::from(result))
    }

    /// 写入语言偏好（`settings_write`）。daemon 随后向所有流式连接推送 `preferences/locale`，
    /// actor 据此更新状态、状态泵再转发给 webview 与原生菜单；应答里的 `effective` 让发起方
    /// 不必等推送就能切换。
    pub async fn set_locale_preference(
        &self,
        update: &UiLocaleUpdate,
    ) -> Result<UiLocalePreference, UiError> {
        let update = match update {
            UiLocaleUpdate::System => LocalePreferenceUpdate::system(),
            UiLocaleUpdate::Explicit { languages } => {
                LocalePreferenceUpdate::explicit(languages.iter().cloned())
            }
        };
        let params = serde_json::to_value(update).map_err(|error| {
            UiError::failed("set_locale_preference", error, "failed", "操作未完成，请稍后重试。")
        })?;
        self.guard("setLocalePreference")?;
        let result: LocalePreferenceResult = self
            .handle
            .request_typed("setLocalePreference", params)
            .await
            .map_err(|error| locale_error("setLocalePreference", error))?;
        Ok(UiLocalePreference::from(result))
    }

    /// 让 actor 重拉一次快照（账户状态、总数）；文件夹与列表由 UI 按需重取。
    pub async fn refresh(&self) -> Result<(), UiError> {
        self.handle
            .refresh()
            .await
            .map_err(|error| UiError::from_client("refresh", error))
    }

    pub async fn frontend_info(&self) -> Result<UiFrontendInfo, UiError> {
        let result: FrontendsResult = self
            .handle
            .request_typed("listFrontends", json!({}))
            .await
            .map_err(|error| UiError::from_client("listFrontends", error))?;
        let me = result
            .frontends
            .into_iter()
            .find(|frontend| frontend.id == self.frontend_id)
            .ok_or_else(|| {
                UiError::not_found("frontend_not_listed", "daemon 的前端清单里没有本前端。")
            })?;
        Ok(UiFrontendInfo {
            is_default: result.default == me.id,
            default_frontend: result.default,
            id: me.id,
            name: me.name,
            description: me.description,
            l10n: me.l10n,
            version: me.version,
            lifecycle: me.lifecycle,
            capabilities: me.capabilities,
            permissions: me.permissions,
        })
    }

    /// 取一条排给本前端的导航意图（ADR-0014 seam）。只能 claim 自己的 id——daemon 侧也校验。
    pub async fn claim_intent(&self) -> Result<Option<OpenIntent>, UiError> {
        let result = self
            .handle
            .claim_next_open_intent(&self.frontend_id)
            .await
            .map_err(|error| UiError::from_client("claimNextOpenIntent", error))?;
        Ok(result.intent)
    }

    /// 诊断：绕过投影的原始请求，仍走认证连接。测试用它证明桥不是 `user`。
    pub async fn raw(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        self.handle.request(method, params).await
    }

    /// 关掉连接 actor。daemon 不受影响——壳没有任何停止后台的调用面（ADR-0031 决策 9）。
    pub async fn shutdown(&self) {
        if let Err(error) = tokio::time::timeout(SHUTDOWN_GRACE, self.handle.shutdown()).await {
            eprintln!("[maild-ui] 连接 actor 未在 {SHUTDOWN_GRACE:?} 内退出: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maild_app::{AccountState, TransportKind};

    fn state_with_error(text: &str) -> ViewState {
        ViewState {
            transport: TransportKind::Unix,
            phase: ConnectionPhase::RetryWaiting {
                attempt: 3,
                delay: Duration::from_millis(1500),
            },
            connection_epoch: 2,
            version: Some("0.1.0".into()),
            accounts: vec![AccountState {
                account: "personal".into(),
                state: "offline".into(),
                detail: "IMAP 登录失败: no response: 认证失败".into(),
                detail_code: Some("login_failed".into()),
                server_detail: "no response: 认证失败".into(),
            }],
            messages: Vec::new(),
            total_messages: 0,
            partial_snapshot: true,
            last_error: Some(text.into()),
            revision: 9,
            principal: None,
            identity_failure: Some(format!("前端包 `{FRONTEND_ID}` 未安装或没有身份条目")),
            identity_failure_code: Some(IdentityFailure::NotInstalled),
            intent_epoch: 0,
            shutdown_requested: false,
            locale: None,
        }
    }

    /// 决策 4：socket 路径、原始异常不出桥。`last_error` 干脆没有对应字段。
    #[test]
    fn snapshot_projection_never_carries_endpoint_paths() {
        let state = state_with_error("连接 Unix socket /var/run/maild/maild.sock 失败: 拒绝连接");
        let snapshot = project(&state, FRONTEND_ID, UiCompat::Unknown);
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("/var/run"), "{json}");
        assert!(!json.contains("拒绝连接"), "{json}");
        assert_eq!(snapshot.phase, "retry_waiting");
        assert_eq!(snapshot.attempt, Some(3));
        assert_eq!(snapshot.retry_in_ms, Some(1500));
        assert_eq!(snapshot.revision, 9);
        assert!(snapshot.partial);
        assert_eq!(snapshot.accounts[0].detail, "IMAP 登录失败: no response: 认证失败");
        // ADR-0048 L6：离线原因的 code 与服务器原文原样过桥。
        assert_eq!(snapshot.accounts[0].detail_code.as_deref(), Some("login_failed"));
        assert_eq!(snapshot.accounts[0].server_detail, "no response: 认证失败");
        assert!(snapshot
            .identity_failure
            .as_deref()
            .unwrap()
            .contains("未安装"));
        assert_eq!(snapshot.identity_failure_code, Some("not_installed"));
    }

    /// ADR-0016：版本不一致时只放行只读请求，新方法默认按写入处理；未知/兼容不挡任何请求。
    #[test]
    fn compat_gate_only_lets_reads_through_on_mismatch() {
        for verdict in [UiCompat::KernelOutdated, UiCompat::AppOutdated] {
            for read in [
                "getMessage",
                "listMessageSummaries",
                "searchMessages",
                "searchContacts",
                "getReadingPolicy",
                "getProxyConfig",
                "whoami",
                "syncMessages",
                "discoverEmailProvider",
                "listEmailPresets",
            ] {
                assert!(!verdict.blocks(read), "{verdict:?} {read}");
            }
            for write in [
                "sendDraft",
                "updateDraft",
                "markMessages",
                "setReadingPolicy",
                "setProxyConfig",
                "testProxy",
                "probeAccount",
                "commitPluginInstall",
                "setDefaultFrontend",
                "fetchServerMessage",
                "setSettings",
                "someFutureMethod",
                // 前缀相近不等于只读：白名单是精确匹配。
                "syncMessagesAndDelete",
            ] {
                assert!(verdict.blocks(write), "{verdict:?} {write}");
            }
            let denial = verdict.denial();
            assert_eq!(denial.code, UiErrorCode::Incompatible);
            assert!(denial.message.contains("修复与后台管理"), "{}", denial.message);
        }
        for verdict in [UiCompat::Unknown, UiCompat::Compatible] {
            assert!(!verdict.blocks("sendDraft"));
        }
        assert_eq!(
            serde_json::to_value(UiCompat::KernelOutdated).unwrap(),
            "kernel_outdated"
        );
        assert_eq!(UiCompat::from(CompatVerdict::AppOutdated), UiCompat::AppOutdated);
    }

    /// `body_html` 不进宿主 webview（M4.5-04 之前没有隔离文档可放）。
    #[test]
    fn message_projection_drops_html_but_reports_it() {
        let message = MailMessage {
            account: "a".into(),
            uid: "1".into(),
            subject: "s".into(),
            body: "text".into(),
            body_html: "<p>hi</p>".into(),
            attachments: vec![MailAttachment {
                part_index: 2,
                filename: "a.pdf".into(),
                mime_type: "application/pdf".into(),
                size: 10,
                ..Default::default()
            }],
            ..Default::default()
        };
        let projected = UiMessage::from(message);
        let json = serde_json::to_value(&projected).unwrap();
        assert!(json.get("body_html").is_none());
        assert_eq!(json["has_html"], true);
        assert_eq!(json["html_bytes"], 9);
        assert_eq!(json["body"], "text");
        assert_eq!(json["attachments"][0]["filename"], "a.pdf");
        assert_eq!(
            json["attachments"][0]["stored"], false,
            "旧 daemon 缺省不制造下载入口"
        );
        assert_eq!(
            json["remote_content"],
            serde_json::json!({ "decision": "block", "reason": "default" })
        );
        // flatten：摘要字段与详情字段同层，UI 用同一个类型读列表行与详情头。
        assert_eq!(json["subject"], "s");
    }

    /// ADR-0032：daemon 的结论原样投影；缺失时回退缺省。
    #[test]
    fn message_projection_carries_the_kernel_remote_content_decision() {
        let result = MessageResult {
            message: MailMessage {
                account: "a".into(),
                uid: "1".into(),
                ..Default::default()
            },
            remote_content: Some(maild_client::RemoteContentDecision {
                decision: "allow".into(),
                reason: "trusted_domain".into(),
            }),
        };
        let projected = UiMessage::from(result);
        assert_eq!(projected.remote_content.decision, "allow");
        assert_eq!(projected.remote_content.reason, "trusted_domain");
    }

    #[test]
    fn client_errors_map_to_four_user_facing_codes() {
        let cases = [
            (
                ClientError::connection("连接 Unix socket /tmp/x 失败"),
                UiErrorCode::Offline,
            ),
            (rpc(-32016), UiErrorCode::Denied),
            (rpc(-32002), UiErrorCode::NotFound),
            (rpc(-32602), UiErrorCode::Invalid),
            (rpc(-32603), UiErrorCode::Failed),
        ];
        for (error, code) in cases {
            let projected = UiError::from_client("test", error);
            assert_eq!(projected.code, code);
            assert!(!projected.message.contains("/tmp"));
        }
    }

    /// ADR-0048 决策 3：webview 按 `reason` 成文；中文 `message` 只是兜底。
    #[test]
    fn errors_carry_a_stable_reason_for_the_webview() {
        let projected = UiError::from_client("x", rpc(-32006));
        assert_eq!(
            serde_json::to_value(&projected).unwrap(),
            json!({
                "code": "conflict",
                "reason": "draft_conflict",
                "message": "草稿已在别处修改。你的输入仍保留，请复制内容或重新载入。"
            })
        );
        assert_eq!(
            UiError::from_client("x", ClientError::connection("down")).reason,
            "offline"
        );
        assert_eq!(UiCompat::KernelOutdated.denial().reason, "kernel_outdated");
        assert_eq!(UiCompat::AppOutdated.denial().reason, "app_outdated");
    }

    /// 老 daemon 不认识语言偏好方法：读时 -32601/-32016 都是「不支持」，写时 -32016 仍是缺授权。
    #[test]
    fn locale_errors_distinguish_an_old_daemon_from_a_denial() {
        for code in [-32601, -32016] {
            let error = locale_error("getLocalePreference", rpc(code));
            assert_eq!((error.code, error.reason), (UiErrorCode::Failed, "locale_unsupported"));
        }
        assert_eq!(
            locale_error("setLocalePreference", rpc(-32601)).reason,
            "locale_unsupported"
        );
        let denied = locale_error("setLocalePreference", rpc(-32016));
        assert_eq!((denied.code, denied.reason), (UiErrorCode::Denied, "denied"));
        assert_eq!(
            locale_error("setLocalePreference", rpc(-32001)).reason,
            "locale_not_persisted"
        );
        assert_eq!(
            locale_error("setLocalePreference", rpc(-32602)).reason,
            "invalid_params"
        );
    }

    #[test]
    fn search_contacts_errors_distinguish_offline_unsupported_and_denied() {
        let offline = search_contacts_error(ClientError::connection("socket down"));
        assert_eq!((offline.code, offline.reason), (UiErrorCode::Offline, "offline"));

        let method_missing = search_contacts_error(rpc(-32601));
        assert_eq!(
            (method_missing.code, method_missing.reason),
            (UiErrorCode::Failed, "contacts_unsupported")
        );

        let legacy_fail_closed = search_contacts_error(ClientError {
            kind: ErrorKind::Rpc,
            message: "插件主体无权调用 searchContacts：该方法仅限用户主体".into(),
            rpc_code: Some(-32016),
            rpc_data: None,
        });
        assert_eq!(
            (legacy_fail_closed.code, legacy_fail_closed.reason),
            (UiErrorCode::Failed, "contacts_unsupported")
        );

        let real_denied = search_contacts_error(ClientError {
            kind: ErrorKind::Rpc,
            message: "插件主体无权调用 searchContacts：缺少 capability messages_read".into(),
            rpc_code: Some(-32016),
            rpc_data: None,
        });
        assert_eq!(
            (real_denied.code, real_denied.reason),
            (UiErrorCode::Denied, "denied")
        );

        let invalid = search_contacts_error(rpc(-32602));
        assert_eq!(
            (invalid.code, invalid.reason),
            (UiErrorCode::Invalid, "invalid_params")
        );
    }

    #[test]
    fn locale_updates_match_the_webview_shape() {
        let system: UiLocaleUpdate = serde_json::from_value(json!({ "mode": "system" })).unwrap();
        assert_eq!(system, UiLocaleUpdate::System);
        let explicit: UiLocaleUpdate =
            serde_json::from_value(json!({ "mode": "explicit", "languages": ["zh-Hans"] }))
                .unwrap();
        assert_eq!(
            explicit,
            UiLocaleUpdate::Explicit {
                languages: vec!["zh-Hans".into()]
            }
        );
        assert!(serde_json::from_value::<UiLocaleUpdate>(json!({ "mode": "auto" })).is_err());
        assert!(serde_json::from_value::<UiLocaleUpdate>(json!({ "mode": "explicit" })).is_err());
        assert_eq!(
            serde_json::to_value(UiLocale { effective: None }).unwrap(),
            json!({ "effective": null })
        );
    }

    fn rpc(code: i64) -> ClientError {
        ClientError {
            kind: ErrorKind::Rpc,
            message: format!("daemon said {code} at /some/path"),
            rpc_code: Some(code),
            rpc_data: None,
        }
    }
}
