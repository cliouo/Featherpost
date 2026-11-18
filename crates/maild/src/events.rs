//! 核心事件协议（对齐 ARCHITECTURE.md §5 的模式约定）。
//!
//! | 事件 | 模式 | 语义 |
//! |---|---|---|
//! | `MailReceived` | waterfall | 过滤链：监听器 `next()` 委托；直接返回即短路丢弃 |
//! | `MailFiltered` | emit | 过滤完成后的观察广播（store / otp 等消费者订阅） |
//! | `OtpDetected` | emit | 验证码提取结果（通知、剪贴板等消费者订阅） |
//! | `AccountState` | emit | 账户连接状态变化（连接中/在线/离线/重连中） |
//! | `NotifyRouteEvent` | bail | 通知路由决策（rules 插件用；R4 定义协议、暂不消费） |
//! | `QuietHoursQuery` | bail | 账户静默时段查询（rules 提供策略，scheduler 只负责刷新时钟） |
//! | `FrontendIntentQueued` | emit | 某前端有新的待领导航意图（ADR-0045：唤醒隐藏在菜单栏的窗口） |
//! | `AgentStoppingEvent` | emit | 用户经本机 IPC 请求整体停止 Agent（推给已认证前端先刷盘草稿并撤下 Dock 图标） |

use cordis::{BailEvent, Context, Disposer, EmitEvent, WaterfallEvent};
use serde::{Deserialize, Serialize};

/// 一个附件 part（ADR-0025）。
///
/// `content` 只在进程内的事件载荷里携带，**不参与序列化**：协议单帧上限 1 MiB，
/// 附件字节走独立取回路径（M4.5-04）。序列化后剩下的是前端排版所需的元数据。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailAttachment {
    /// MIME 树深度优先遍历序，(account, uid) 内稳定。
    pub part_index: u32,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub mime_type: String,
    /// `Content-ID`（尖括号已剥离），供正文里的 `cid:` 引用。
    #[serde(default)]
    pub content_id: String,
    /// 内联（正文引用的图片等），不点亮「有附件」。
    #[serde(default)]
    pub inline: bool,
    /// 解码后字节数；即使内容超限未落库也如实记录。
    pub size: u64,
    /// 字节是否已落库（ADR-0032 决策 3）：`false` = 超过内联上限或迁移前的历史缓存，
    /// `readAttachment` 会得到 `-32004`。UI 据此决定要不要画一个能点的下载入口，
    /// 而不是先做按钮再让它失败。旧 daemon 缺省 `false`——不确定时不制造入口。
    #[serde(default)]
    pub stored: bool,
    /// 解码后的字节；`None` = 超过内联上限，需按需回源。
    #[serde(skip)]
    pub content: Option<Vec<u8>>,
}

/// 收到的邮件（进过滤链的载荷）。
///
/// 派生 `Default` 是为下游可演进性：ADR-0025 之后字段较多，让每个构造点逐字段书写
/// 会让「加一个字段」变成跨 crate 的机械改写，实际后果是字段被塞进 `body` 里凑合。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MailMessage {
    /// 账户名（receive 配置里的 account）。
    pub account: String,
    /// IMAP UID。
    pub uid: String,
    pub sender: String,
    /// 收件人（To 头首值；ADR-0022 地址层溯源用，可能因转发链失真，
    /// 仅作展示性元数据）。provider 合同缺省为空串。
    #[serde(default)]
    pub recipient: String,
    pub subject: String,
    /// 提取出的纯文本正文（优先 text/plain，否则由 HTML 转换而来）。
    pub body: String,
    /// **本机第一次观测到这封信的时刻**（unix 秒），不是发信时间。
    /// 发信时间见 [`MailMessage::date_at`]，排序键见 [`MailMessage::sort_at`]。
    pub received_at: u64,

    // —— ADR-0025：RFC 5322 事实、HTML 原文、摘要与标记位。全部 additive。——
    /// `Message-ID`（尖括号已剥离）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub in_reply_to: String,
    /// `References` 头原文，空格分隔（store 列名为 `reference_ids`：
    /// `references` 是 SQLite 关键字）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub references: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cc: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bcc: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reply_to: String,
    /// 解析成功的 `Date` 头（unix 秒）；缺失或不可解析为 `None`。
    /// 原样保留——即使它不可信到不能参与排序。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_at: Option<u64>,
    /// 列表排序键：可信的 `date_at`，否则 `received_at`（ADR-0025 决策 3）。
    #[serde(default)]
    pub sort_at: u64,
    /// HTML 原文，有界。`listMessages` 与 `message/received` 通知**不携带**它。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body_html: String,
    /// 列表预览（正文前若干字符，空白折叠）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub snippet: String,
    /// 是否存在非内联附件。
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
    /// 附件（含内联）。字节只在进程内，见 [`MailAttachment::content`]。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<MailAttachment>,

    // —— ADR-0026：一封邮件的身份是 (account, folder, uid)。——
    /// 所属文件夹的**服务器线上原名**（可能是 modified UTF-7）。空 = 账户的 `INBOX`。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// 所属文件夹的角色（inbox/sent/drafts/trash/archive/spam），无角色为空串。
    /// 读取路径填充；写入路径忽略（以 `folders` 表为准）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder_role: String,
    // —— ADR-0029：线程聚合。——
    /// 所属线程 id（`Message-ID` 图的连通分量，账户内唯一）。
    /// 尚未回填索引的历史消息为 0。
    #[serde(default)]
    pub thread_id: i64,
}

/// 邮件列表使用的轻量摘要。完整正文只通过 `getMessage` 按需读取。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MessageSummary {
    pub account: String,
    pub uid: String,
    pub sender: String,
    #[serde(default)]
    pub recipient: String,
    pub subject: String,
    pub received_at: u64,

    // —— ADR-0025 ——
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
    // —— ADR-0029：线程聚合。——
    /// 所属线程 id（`Message-ID` 图的连通分量，账户内唯一）。
    /// 尚未回填索引的历史消息为 0。
    #[serde(default)]
    pub thread_id: i64,
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

/// 过滤决策。
#[derive(Clone, Debug, PartialEq)]
pub enum FilterDecision {
    /// 通过过滤链（继续投递 store / otp 等）。
    Accept,
    /// 丢弃。
    Reject,
}

/// `mail/received`：过滤链（waterfall）。
pub struct MailReceived;

impl WaterfallEvent for MailReceived {
    type Args = MailMessage;
    type Ret = FilterDecision;
}

/// `mail/filtered`：过滤完成后的观察广播。
pub struct MailFiltered;

impl EmitEvent for MailFiltered {
    type Args = MailMessage;
}

/// 验证码提取命中。
#[derive(Clone, Debug, PartialEq)]
pub struct OtpHit {
    pub code: String,
    pub from: String,
    pub subject: String,
    pub account: String,
    pub uid: String,
}

/// `otp/detected`：验证码提取结果。
pub struct OtpDetected;

impl EmitEvent for OtpDetected {
    type Args = OtpHit;
}

/// 账户连接状态。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountStateKind {
    Connecting,
    Online,
    Offline,
    Reconnecting,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountStateInfo {
    pub account: String,
    pub state: AccountStateKind,
    /// 附加信息（如断线原因）。
    pub detail: String,
}

/// `account/state`：账户连接状态变化。
pub struct AccountState;

impl EmitEvent for AccountState {
    type Args = AccountStateInfo;
}

/// 通知路由（bail 决策的载荷与返回值：策略监听器直接返回最终通道）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotifyRoute {
    /// 最终通道："system"（系统通知）| "silent"（静默，仅记录/跳过）。
    pub channel: String,
    /// 是否含敏感内容（通知总开关 + 预览策略用）。
    pub sensitive: bool,
    /// 通知标题（路由决策用；简化：并入载荷而非独立 RouteCtx）。
    pub subject: String,
    /// 来源（路由决策用）。
    pub from: String,
}

/// `notify/route`：通知路由决策。
pub struct NotifyRouteEvent;

impl BailEvent for NotifyRouteEvent {
    type Args = NotifyRoute;
    type Ret = NotifyRoute;
}

/// 静默时段查询参数。rules 提供策略，scheduler 不解释窗口内容。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuietHoursRequest {
    pub account: String,
    pub at_unix_secs: u64,
}

/// `quiet-hours/query`：首个策略监听器返回最终是否静默。
pub struct QuietHoursQuery;

impl BailEvent for QuietHoursQuery {
    type Args = QuietHoursRequest;
    type Ret = bool;
}

/// `frontend/intent`：`FrontendBroker` 为某个前端入队了一条导航意图（ADR-0045）。
///
/// 载荷只有目标 frontend id——意图本身仍只经 `claimFrontendIntent` 一次性领取，推送帧不携带
/// 任何路由数据。API 层只把它投递给主体恰好是 `frontend:<id>` 的长连接，用来唤醒隐藏在
/// 菜单栏（`Accessory`）里、收不到 `Reopen` 的窗口。
pub struct FrontendIntentQueued;

impl EmitEvent for FrontendIntentQueued {
    type Args = String;
}

/// 事件总线上 [`AgentStoppingEvent`] 的稳定名称（日志/文档引用用；投递按类型分发）。
pub const EVENT_AGENT_STOPPING: &str = "maild.agent.stopping";

/// `agent/stopping`：用户经本机 IPC 调用 `stopManagedAgent`，Agent 即将整体停机。
///
/// 在 `ManagedAgentControl` 的排空窗口开始前广播；API 层只把它推给主体为
/// `Principal::Frontend(_)` 的已认证长连接，让正在打开/最小化的窗口先把未保存草稿刷盘、
/// 切回 `Accessory` 撤下 Dock 图标后再自行退出。载荷为空：推送帧不携带任何状态。
#[derive(Clone, Debug, Default)]
pub struct AgentStoppingEvent;

impl EmitEvent for AgentStoppingEvent {
    type Args = AgentStoppingEvent;
}

/// Agent 生命周期事件的便捷入口（对 [`Context`] 的扩展）。
pub trait AgentLifecycleEvents {
    /// 广播 [`AgentStoppingEvent`]（同步投递给当前全部监听器）。
    fn emit_agent_stopping(&self);
    /// 订阅 [`AgentStoppingEvent`]；返回的 disposer 撤销订阅。
    fn on_agent_stopping(
        &self,
        handler: impl Fn(AgentStoppingEvent) + Send + Sync + 'static,
    ) -> Disposer;
}

impl AgentLifecycleEvents for Context {
    fn emit_agent_stopping(&self) {
        self.emit::<AgentStoppingEvent>(AgentStoppingEvent);
    }

    fn on_agent_stopping(
        &self,
        handler: impl Fn(AgentStoppingEvent) + Send + Sync + 'static,
    ) -> Disposer {
        self.on_emit::<AgentStoppingEvent>(handler)
    }
}
