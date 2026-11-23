//! receive：每账户一条 imap-idle 长连接，断线退避重连。
//!
//! 新邮件 → `MailReceived` waterfall（过滤链；R4 无 filters 插件，终端默认 Accept）
//! → 接受后发 `MailFiltered`。
//!
//! 依赖 tokio 运行时上下文（守护进程内运行）；`apply` 时无运行时则报错（fiber 置 Idle）。
//! 重连重放安全：已处理 UID 去重（内存态；重启重放由 store 的 (account,uid) 唯一键兜底，
//! otp 的 (account,uid) 去重防重复通知）。
//!
//! 低占用约束（ADR-0041）：会话内去重按文件夹有界（O(folders) 常驻内存）；冷启动只拉最新
//! `initial_sync_limit` 封正文并按 [`FETCH_BATCH_SIZE`] 分批；连接带 TCP keepalive 与分阶段
//! 超时；断线按指数退避重连，且退避可被 [`Receive::trigger_sync`] 立即打断。

use std::any::TypeId;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_imap::Client;
use cordis::{Context, Plugin, PluginError, RealmId, Service};
use futures::StreamExt;
use maild_client::{
    TransportReceiveRequest, TransportReceiveResponse,
};
use mailparse::{parse_mail, MailHeaderMap};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};

use crate::config::Merge;
use crate::credential::SecretBuffer;
use crate::events::{
    AccountDetailCode, AccountState, AccountStateInfo, AccountStateKind, FilterDecision,
    MailFiltered, MailMessage, MailReceived, MessageSummary,
};
use crate::folders::{self, FolderRole};
use crate::pending::{ObservedFlags, OpKind, OpState, PendingOp};
use crate::mime;
use crate::scheduler::{InitialDelay, JitterPolicy, ScheduleError, ScheduleSpec, Scheduler};
use crate::store::Store;

/// async-imap 要求的连接：tokio 异步读写 + Debug（组合 trait 对象）。
pub(crate) trait ImapIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + std::fmt::Debug {}

impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + std::fmt::Debug> ImapIo
    for T
{
}

pub(crate) type Conn = Box<dyn ImapIo>;

/// 每次连接尝试前取回秘密的回调。受管账户经 Keychain 取入短生命周期缓冲，
/// 秘密不进入长期 `ReceiveConfig`（ADR-0015 §5）。
pub type SecretFetcher = Arc<dyn Fn() -> Result<SecretBuffer, ReceiveError> + Send + Sync>;

/// 收信循环的错误（ADR-0048 L6）：稳定 code + 服务器/系统原文。
///
/// `Display` 是今天的中文整句，日志与账户状态的 `detail` 逐字节不变；前端改按 `code` 成文，
/// `server_detail` 作为数据原样展示。原文全由我们自己判断（超时、凭据库、本地库、装配缺失）
/// 时 `server_detail` 为空——那些文字是界面文案，不是数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiveError {
    code: AccountDetailCode,
    message: String,
    server_detail: String,
}

impl ReceiveError {
    /// 整句都是我们写的：没有外来原文。
    pub fn new(code: AccountDetailCode, message: impl Into<String>) -> Self {
        ReceiveError {
            code,
            message: message.into(),
            server_detail: String::new(),
        }
    }

    /// `{prefix}: {detail}`：`prefix` 是界面文字，`detail` 是服务器或系统原文。
    pub fn with_detail(
        code: AccountDetailCode,
        prefix: impl std::fmt::Display,
        detail: impl std::fmt::Display,
    ) -> Self {
        let detail = detail.to_string();
        ReceiveError {
            code,
            message: format!("{prefix}: {detail}"),
            server_detail: crate::probe::sanitize_detail(&detail),
        }
    }

    pub fn code(&self) -> AccountDetailCode {
        self.code
    }

    pub fn server_detail(&self) -> &str {
        &self.server_detail
    }
}

impl std::fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ReceiveError {}

// —— 按阶段的 `map_err` 辅助：一处决定 code，各调用点只写前缀 ——

/// 带服务器/系统原文的阶段失败：`.map_err(stage(code, "前缀"))` 得到 `前缀: 原文`。
fn stage<E: std::fmt::Display, P: std::fmt::Display>(
    code: AccountDetailCode,
    prefix: P,
) -> impl FnOnce(E) -> ReceiveError {
    move |error| ReceiveError::with_detail(code, prefix, error)
}

/// IMAP 同步命令（LIST / SELECT / FETCH / 对账）失败。
fn sync_stage<E: std::fmt::Display, P: std::fmt::Display>(
    prefix: P,
) -> impl FnOnce(E) -> ReceiveError {
    stage(AccountDetailCode::SyncFailed, prefix)
}

/// 本地库（游标、modseq、对账写回）失败：整句仍是 `前缀: 报错`，但报错是我们自己的文字，
/// 不作为 `server_detail` 透出。归 `sync_failed`（ADR-0048 L6 计划：游标属于同步阶段）。
fn store_stage<E: std::fmt::Display, P: std::fmt::Display>(
    prefix: P,
) -> impl FnOnce(E) -> ReceiveError {
    move |error| ReceiveError::new(AccountDetailCode::SyncFailed, format!("{prefix}: {error}"))
}

/// 整句是固定文案的失败（超时、我们自己的判断）：丢弃底层错误值。
fn fixed<E>(code: AccountDetailCode, message: &'static str) -> impl FnOnce(E) -> ReceiveError {
    move |_| ReceiveError::new(code, message)
}

/// STARTTLS 协商逐字节读行的 IO 失败。`read_line_exact` 在对端提前关闭时自己写了一句中文，
/// 那不是系统原文，不透出。
fn line_stage<P: std::fmt::Display>(
    code: AccountDetailCode,
    prefix: P,
) -> impl FnOnce(tokio::io::Error) -> ReceiveError {
    move |error| {
        if error.kind() == tokio::io::ErrorKind::UnexpectedEof {
            ReceiveError::new(code, format!("{prefix}: {error}"))
        } else {
            ReceiveError::with_detail(code, prefix, error)
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReceiveConfig {
    /// 账户名（事件载荷用；多账户各配一个 receive 实例）。
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub server: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_mailbox")]
    pub mailbox: String,
    /// 断线重连间隔（秒）。
    #[serde(default = "default_reconnect_secs")]
    pub reconnect_secs: u64,
    /// false → 轮询兜底（poll_secs 间隔；供不支持 IDLE 的服务器）。
    #[serde(default = "default_idle")]
    pub idle: bool,
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// 非主文件夹的 sweep 周期（秒）。IDLE 只挂在主文件夹上，其余文件夹在同一条连接上
    /// 周期性增量同步（ADR-0026 决策 3）。
    #[serde(default = "default_folder_poll_secs")]
    pub folder_poll_secs: u64,
    /// "none"（默认，明文）| "starttls"（手动协商升级）| "implicit"（993 直连 TLS）。
    #[serde(default = "default_tls")]
    pub tls: String,
    /// 跳过证书校验（仅测试/自签场景；生产必须 false，走系统根证书）。
    #[serde(default)]
    pub accept_invalid_certs: bool,
    /// 冷启动（文件夹尚无持久游标）时最多拉取多少封最新邮件的正文（ADR-0041 决策 1）。
    /// 更早的历史走服务器检索 + [`Receive::fetch_message_by_uid`] 按需拉取。`0` = 不限制。
    #[serde(default = "default_initial_sync_limit")]
    pub initial_sync_limit: usize,
}

fn default_port() -> u16 {
    143
}
fn default_mailbox() -> String {
    "INBOX".into()
}
fn default_reconnect_secs() -> u64 {
    5
}
fn default_idle() -> bool {
    true
}
fn default_poll_secs() -> u64 {
    60
}
fn default_folder_poll_secs() -> u64 {
    300
}
fn default_tls() -> String {
    "none".into()
}
fn default_initial_sync_limit() -> usize {
    DEFAULT_INITIAL_SYNC_LIMIT
}

/// 冷启动同步窗口的默认值（ADR-0041 决策 1）。
pub const DEFAULT_INITIAL_SYNC_LIMIT: usize = 200;
/// 一条 `UID FETCH (UID FLAGS BODY.PEEK[])` 最多携带的 UID 数（ADR-0041 决策 1）。
pub const FETCH_BATCH_SIZE: usize = 50;
/// 冷启动发生截断时，只有最新这么多封走实时通知链路，其余直接静默落库。
const COLD_START_NOTIFY_TAIL: usize = 5;
/// 每个文件夹会话内去重环的容量：只为吸收 IDLE 唤醒重叠，不是历史记录。
const SESSION_DEDUP_CAPACITY: usize = 256;

impl Service for ReceiveConfig {
    fn name(&self) -> &'static str {
        "receive.config"
    }
}

impl Default for ReceiveConfig {
    /// 与 serde 默认值一致（防 `Default`/反序列化两条路径漂移）。
    fn default() -> Self {
        ReceiveConfig {
            account: String::new(),
            server: String::new(),
            port: default_port(),
            username: String::new(),
            password: String::new(),
            mailbox: default_mailbox(),
            reconnect_secs: default_reconnect_secs(),
            idle: default_idle(),
            poll_secs: default_poll_secs(),
            folder_poll_secs: default_folder_poll_secs(),
            tls: default_tls(),
            accept_invalid_certs: false,
            initial_sync_limit: default_initial_sync_limit(),
        }
    }
}

impl Merge for ReceiveConfig {
    /// 字段级覆盖（空/零值 = 不覆盖）。
    fn merge(&self, frag: &Self) -> Self {
        let or = |a: &str, b: &str| {
            if b.is_empty() {
                a.to_string()
            } else {
                b.to_string()
            }
        };
        ReceiveConfig {
            account: or(&self.account, &frag.account),
            server: or(&self.server, &frag.server),
            port: if frag.port == 0 { self.port } else { frag.port },
            username: or(&self.username, &frag.username),
            password: or(&self.password, &frag.password),
            mailbox: or(&self.mailbox, &frag.mailbox),
            reconnect_secs: if frag.reconnect_secs == 0 {
                self.reconnect_secs
            } else {
                frag.reconnect_secs
            },
            idle: self.idle || frag.idle,
            poll_secs: if frag.poll_secs == 0 {
                self.poll_secs
            } else {
                frag.poll_secs
            },
            folder_poll_secs: if frag.folder_poll_secs == 0 {
                self.folder_poll_secs
            } else {
                frag.folder_poll_secs
            },
            tls: or(&self.tls, &frag.tls),
            accept_invalid_certs: self.accept_invalid_certs || frag.accept_invalid_certs,
            // 与其他数值字段同一约定：零值 = 不覆盖。
            initial_sync_limit: if frag.initial_sync_limit == 0 {
                self.initial_sync_limit
            } else {
                frag.initial_sync_limit
            },
        }
    }
}

/// 账户连接状态句柄（api 插件 / 健康检查 / e2e 等待用）。
#[derive(Clone)]
pub struct Receive {
    inner: Arc<ReceiveInner>,
}

impl Service for Receive {
    fn name(&self) -> &'static str {
        "receive"
    }
}

struct ReceiveInner {
    account: String,
    state: Mutex<AccountStateInfo>,
    online: watch::Sender<bool>,
    shutdown: watch::Sender<bool>,
    /// 写路径的唤醒计数（ADR-0027 决策 8）：API 只入队 + 敲一下，
    /// 命令的实际执行发生在收信循环里。
    wake: watch::Sender<u64>,
    /// 服务器侧 `SEARCH` 的请求投递口（ADR-0029 决策 11）。形状与 wake 同源，
    /// 区别是它要回传结果，所以每条请求自带一个 oneshot。
    search: mpsc::Sender<ServerSearchRequest>,
    /// 接收端由 `serve` 独占——同一时刻只有一条连接在跑。
    search_rx: tokio::sync::Mutex<mpsc::Receiver<ServerSearchRequest>>,
    /// 「立即同步」计数（ADR-0041 决策 4）：打断 IDLE / 轮询等待 / 断线退避睡眠。
    sync_wake: watch::Sender<u64>,
    manual_sync: Mutex<Option<ManualSync>>,
    /// 按需单封拉取的投递口（ADR-0041 决策 4），与 `search` 同构。
    fetch: mpsc::Sender<FetchByUidRequest>,
    fetch_rx: tokio::sync::Mutex<mpsc::Receiver<FetchByUidRequest>>,
}

struct ManualSync {
    id: u64,
    queued: bool,
    folder: Option<String>,
    outcome: Option<Result<(), String>>,
}

static NEXT_MANUAL_SYNC: AtomicU64 = AtomicU64::new(1);

/// 一次按需单封拉取请求。
pub(crate) struct FetchByUidRequest {
    folder: String,
    uid: u32,
    reply: oneshot::Sender<Result<Option<MailMessage>, String>>,
}

/// 按需拉取队列容量：有界，满了立刻拒绝而不是无限排队。
const FETCH_BY_UID_QUEUE: usize = 4;

/// 一次服务器侧检索请求。
pub(crate) struct ServerSearchRequest {
    folder: String,
    query: String,
    since: Option<u64>,
    until: Option<u64>,
    reply: oneshot::Sender<Result<Vec<MessageSummary>, String>>,
}

/// 服务器检索的总时限。收信循环可能正卡在一次同步里，但用户不该无限等。
const SERVER_SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
/// 按需单封拉取的总时限（同上）。
const FETCH_BY_UID_TIMEOUT: Duration = Duration::from_secs(30);

// —— 连接阶段超时与 keepalive（ADR-0041 决策 3）——
/// 单个解析地址的 TCP 连接上限。
const CONNECT_PER_ADDR_TIMEOUT: Duration = Duration::from_secs(10);
/// 整个 TCP 连接阶段（含 DNS 与多地址逐个尝试）的上限。
const CONNECT_TOTAL_TIMEOUT: Duration = Duration::from_secs(20);
/// TLS / STARTTLS 握手上限。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_INIT_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_DONE_TIMEOUT: Duration = Duration::from_secs(10);
/// 单次 IDLE 的最长等待：RFC 2177 建议 ≤29 分钟续期，留余量取 25 分钟。
const IDLE_RENEW_INTERVAL: Duration = Duration::from_secs(25 * 60);
/// TCP keepalive 空闲探测起点（秒）。
#[cfg(unix)]
const TCP_KEEPALIVE_IDLE_SECS: libc::c_int = 120;
/// 断线退避上限与指数封顶。
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(300);
const RECONNECT_BACKOFF_MAX_EXPONENT: u32 = 6;

/// 分阶段超时的一份取值。生产恒为上面的常量；单测在本线程临时缩短，以确定性地走到各个超时
/// 分支（ADR-0048 L6 要求每个 code 都有触发路径）。
#[derive(Clone, Copy, Debug)]
struct StageTimeouts {
    connect_per_addr: Duration,
    connect_total: Duration,
    handshake: Duration,
    login: Duration,
    idle_init: Duration,
    idle_done: Duration,
    /// `None` = 用 provider 模块的反向调用预算。
    provider_call: Option<Duration>,
}

const STAGE_TIMEOUTS: StageTimeouts = StageTimeouts {
    connect_per_addr: CONNECT_PER_ADDR_TIMEOUT,
    connect_total: CONNECT_TOTAL_TIMEOUT,
    handshake: HANDSHAKE_TIMEOUT,
    login: LOGIN_TIMEOUT,
    idle_init: IDLE_INIT_TIMEOUT,
    idle_done: IDLE_DONE_TIMEOUT,
    provider_call: None,
};

#[cfg(not(test))]
fn stage_timeouts() -> StageTimeouts {
    STAGE_TIMEOUTS
}

#[cfg(test)]
thread_local! {
    static TEST_STAGE_TIMEOUTS: std::cell::Cell<Option<StageTimeouts>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn stage_timeouts() -> StageTimeouts {
    TEST_STAGE_TIMEOUTS.with(|cell| cell.get()).unwrap_or(STAGE_TIMEOUTS)
}

fn provider_call_budget() -> Duration {
    stage_timeouts()
        .provider_call
        .unwrap_or_else(crate::provider::receive_call_budget)
}

impl Receive {
    pub(crate) fn new(account: &str) -> Receive {
        let (online, _) = watch::channel(false);
        let (shutdown, _) = watch::channel(false);
        let (wake, _) = watch::channel(0);
        // 容量 1：同一时刻只允许一次服务器检索在途，第二次立刻得到明确拒绝而不是排队。
        let (search, search_rx) = mpsc::channel(1);
        let (sync_wake, _) = watch::channel(0);
        let (fetch, fetch_rx) = mpsc::channel(FETCH_BY_UID_QUEUE);
        Receive {
            inner: Arc::new(ReceiveInner {
                account: account.to_string(),
                wake,
                search,
                search_rx: tokio::sync::Mutex::new(search_rx),
                sync_wake,
                manual_sync: Mutex::new(None),
                fetch,
                fetch_rx: tokio::sync::Mutex::new(fetch_rx),
                state: Mutex::new(AccountStateInfo::plain(account, AccountStateKind::Connecting)),
                online,
                shutdown,
            }),
        }
    }

    pub fn state(&self) -> AccountStateInfo {
        self.inner.state.lock().unwrap().clone()
    }

    /// 等到首次在线（e2e 同步点；超时由调用方包一层 `tokio::time::timeout`）。
    pub async fn wait_online(&self) {
        let mut rx = self.inner.online.subscribe();
        if *rx.borrow() {
            return;
        }
        let _ = rx.wait_for(|v| *v).await;
    }

    /// 当前连接是否处于 `Online`（已登录、已完成文件夹发现）。
    pub fn is_online(&self) -> bool {
        *self.inner.online.borrow()
    }

    fn set_online(&self, v: bool) {
        // send_replace 无条件更新（无 receiver 也保留值）：wait_online 的 receiver
        // 可能在 send 之后才 subscribe，用 send 会丢值导致 wait_for 死等。
        let _ = self.inner.online.send_replace(v);
        if !v {
            self.cancel_manual_sync();
        }
    }

    /// 没有原因的状态（连接中、在线、重连中）。
    fn set_state(&self, ctx: &Context, state: AccountStateKind) {
        self.publish(ctx, AccountStateInfo::plain(self.inner.account.clone(), state));
    }

    /// 离线并附原因：`detail` 是今天的整句，另带稳定 code 与服务器原文（ADR-0048 L6）。
    fn set_offline(&self, ctx: &Context, error: &ReceiveError) {
        self.publish(
            ctx,
            AccountStateInfo {
                account: self.inner.account.clone(),
                state: AccountStateKind::Offline,
                detail: error.to_string(),
                detail_code: Some(error.code()),
                server_detail: error.server_detail().to_string(),
            },
        );
    }

    fn publish(&self, ctx: &Context, info: AccountStateInfo) {
        *self.inner.state.lock().unwrap() = info.clone();
        ctx.emit::<AccountState>(info);
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.inner.shutdown.send_replace(true);
        self.cancel_manual_sync();
    }

    /// 敲醒收信循环去清写队列。账户离线时是空操作——op 留在库里等下次连接。
    pub fn wake_writes(&self) {
        let next = self.inner.wake.borrow().wrapping_add(1);
        let _ = self.inner.wake.send_replace(next);
    }

    fn wake_rx(&self) -> watch::Receiver<u64> {
        self.inner.wake.subscribe()
    }

    /// 在收信循环那条连接上跑一次服务器侧 `SEARCH`（ADR-0029 决策 11）。
    ///
    /// 账户离线时立即返回明确错误，而不是把请求排进一个不知道什么时候会被消费的队列。
    pub async fn server_search(
        &self,
        folder: &str,
        query: &str,
        since: Option<u64>,
        until: Option<u64>,
    ) -> Result<Vec<MessageSummary>, String> {
        if !*self.inner.online.borrow() {
            return Err("账户当前离线，无法执行服务器检索".into());
        }
        let (reply, wait) = oneshot::channel();
        self.inner
            .search
            .try_send(ServerSearchRequest {
                folder: folder.to_string(),
                query: query.to_string(),
                since,
                until,
                reply,
            })
            .map_err(|_| "已有一次服务器检索在执行，请稍后重试".to_string())?;
        match tokio::time::timeout(SERVER_SEARCH_TIMEOUT, wait).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("服务器检索被中止：连接已断开".into()),
            Err(_) => Err("服务器检索超时".into()),
        }
    }

    /// 立即唤醒收信循环跑一轮增量同步（含非主文件夹 sweep）与写队列排空（ADR-0041 决策 4）。
    ///
    /// 打断三种等待：IDLE、轮询间隔、断线退避睡眠——用户点「立即同步」不必等退避倒计时。
    /// 循环正忙于一次同步时，信号会在这次同步结束后立刻生效。
    pub fn trigger_sync(&self) {
        self.inner
            .sync_wake
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    pub(crate) fn start_manual_sync(&self, folder: Option<String>) -> Result<u64, String> {
        let mut state = self.inner.manual_sync.lock().unwrap();
        if self.is_shutdown() {
            return Err("账户当前离线，无法同步".into());
        }
        if state.as_ref().is_some_and(|sync| sync.outcome.is_none()) {
            return Err("账户正在同步".into());
        }
        let id = NEXT_MANUAL_SYNC.fetch_add(1, Ordering::Relaxed);
        *state = Some(ManualSync {
            id,
            queued: true,
            folder,
            outcome: None,
        });
        self.trigger_sync();
        Ok(id)
    }

    pub(crate) fn manual_sync_done(&self, id: u64) -> Result<bool, String> {
        let state = self.inner.manual_sync.lock().unwrap();
        match state.as_ref().filter(|sync| sync.id == id) {
            Some(sync) => match &sync.outcome {
                None => Ok(false),
                Some(outcome) => outcome.clone().map(|()| true),
            },
            None => Err("同步请求已失效".into()),
        }
    }

    fn take_manual_sync(&self) -> Option<(u64, Option<String>)> {
        let mut state = self.inner.manual_sync.lock().unwrap();
        let sync = state
            .as_mut()
            .filter(|sync| sync.queued && sync.outcome.is_none())?;
        sync.queued = false;
        Some((sync.id, sync.folder.take()))
    }

    fn has_queued_manual_sync(&self) -> bool {
        self.inner
            .manual_sync
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|sync| sync.queued && sync.outcome.is_none())
    }

    fn cancel_manual_sync(&self) {
        let mut state = self.inner.manual_sync.lock().unwrap();
        if let Some(sync) = state.as_mut().filter(|sync| sync.outcome.is_none()) {
            sync.outcome = Some(Err("同步被中止：连接已断开".into()));
        }
    }

    fn finish_manual_sync(&self, id: u64, outcome: Result<(), String>) {
        let mut state = self.inner.manual_sync.lock().unwrap();
        if let Some(sync) = state
            .as_mut()
            .filter(|sync| sync.id == id && sync.outcome.is_none())
        {
            sync.outcome = Some(outcome);
        }
    }

    fn sync_rx(&self) -> watch::Receiver<u64> {
        self.inner.sync_wake.subscribe()
    }

    /// 在收信循环那条连接上按需拉取一封邮件的完整报文并幂等落库（ADR-0041 决策 4）。
    ///
    /// 冷启动只同步最新一段窗口，更早的历史经服务器检索定位后由这里补齐。服务器上该 UID
    /// 不存在时返回 `Ok(None)`。与 [`Receive::server_search`] 一样不新建连接。
    pub async fn fetch_message_by_uid(
        &self,
        folder: &str,
        uid: u32,
    ) -> Result<Option<MailMessage>, String> {
        if uid == 0 {
            return Err("UID 必须是正整数".into());
        }
        if !self.is_online() {
            return Err("账户当前离线，无法按需拉取邮件".into());
        }
        let (reply, wait) = oneshot::channel();
        self.inner
            .fetch
            .try_send(FetchByUidRequest {
                folder: folder.to_string(),
                uid,
                reply,
            })
            .map_err(|_| "按需拉取请求过多，请稍后重试".to_string())?;
        match tokio::time::timeout(FETCH_BY_UID_TIMEOUT, wait).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("按需拉取被中止：连接已断开".into()),
            Err(_) => Err("按需拉取超时".into()),
        }
    }

    /// 断线退避睡眠：到期、`trigger_sync` 或关断任一发生即返回。返回 false = 已关断。
    async fn backoff_sleep(&self, delay: Duration) -> bool {
        let mut shutdown_rx = self.shutdown_rx();
        let mut sync_rx = self.sync_rx();
        if self.is_shutdown() {
            return false;
        }
        if self.has_queued_manual_sync() {
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = sync_rx.changed() => {}
            _ = shutdown_rx.changed() => {}
        }
        !self.is_shutdown()
    }

    fn is_shutdown(&self) -> bool {
        *self.inner.shutdown.borrow()
    }

    fn shutdown_rx(&self) -> watch::Receiver<bool> {
        self.inner.shutdown.subscribe()
    }
}

/// receive 插件：inject 配置 → provide Receive → 启动 IDLE 任务。
pub struct ReceivePlugin;

impl Plugin for ReceivePlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<ReceiveConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "receive 插件需要在 tokio 运行时内挂载（用守护进程运行）")?;
        let cfg = ctx.get::<ReceiveConfig>().expect("inject 就绪");
        let scheduler = if cfg.idle {
            None
        } else {
            Some(
                ctx.get::<Scheduler>()
                    .ok_or("轮询 receive 需要先装配 scheduler 插件")?
                    .as_ref()
                    .clone(),
            )
        };

        let receive = Receive::new(&cfg.account);
        ctx.provide::<Receive>(Arc::new(receive.clone()));

        let ctx2 = ctx.clone();
        let task = handle.spawn(run_loop(
            cfg.as_ref().clone(),
            ctx2,
            receive.clone(),
            scheduler,
            None,
        ));

        // 卸载逆：关断信号 → 等任务退出
        ctx.effect_async(async move {
            receive.shutdown();
            let _ = task.await;
        });
        Ok(())
    }
}

/// 连接生命周期：连上服务 → 断线 → 退避重连，直到关断。
///
/// `secret_fetcher` 为 Some 时（受管账户），每次连接尝试前重新取回秘密；
/// 为 None 时使用旧配置内嵌密码（legacy / hermetic 测试路径）。
async fn run_loop(
    cfg: ReceiveConfig,
    ctx: Context,
    receive: Receive,
    scheduler: Option<Scheduler>,
    secret_fetcher: Option<SecretFetcher>,
) {
    let mut first = true;
    // 连续未能进入 Online 的次数；驱动指数退避（ADR-0041 决策 3）。
    let mut consecutive_failures: u32 = 0;
    loop {
        if receive.is_shutdown() {
            return;
        }
        let state = if first {
            AccountStateKind::Connecting
        } else {
            AccountStateKind::Reconnecting
        };
        receive.set_state(&ctx, state);
        first = false;

        // 钥匙串读取是同步阻塞的系统调用：`SecItemCopyMatching` 要等 securityd，
        // 钥匙串锁定或 Agent 签名变更时还会弹授权窗。守护进程跑在 tokio
        // current_thread 上，直接在这里调用会把整个运行时钉死——连 API socket
        // 都不再应答（2026-08-28 实测到过一次）。所以移出运行时线程。
        let fetched: Result<SecretBuffer, ReceiveError> = match &secret_fetcher {
            Some(fetch) => {
                let fetch = Arc::clone(fetch);
                tokio::task::spawn_blocking(move || fetch())
                    .await
                    .unwrap_or_else(|_| {
                        Err(ReceiveError::new(
                            AccountDetailCode::CredentialUnavailable,
                            "凭据读取任务失败",
                        ))
                    })
            }
            None => Ok(SecretBuffer::new(cfg.password.clone().into_bytes())),
        };
        let secret: SecretBuffer = match fetched {
            Ok(secret) => secret,
            Err(error) => {
                // 凭据读取失败是可恢复状态：不回滚配置，退避后重试。
                receive.set_offline(&ctx, &error);
                receive.set_online(false);
                let delay = reconnect_backoff(cfg.reconnect_secs, consecutive_failures);
                consecutive_failures = consecutive_failures.saturating_add(1);
                if !receive.backoff_sleep(delay).await {
                    return;
                }
                continue;
            }
        };

        match serve(&cfg, &ctx, &receive, scheduler.as_ref(), secret).await {
            Ok(()) => return, // 正常关断
            Err(e) => {
                // 这次会话曾经上线 = 网络本身可用，断线是偶发的：退避从头计。
                if receive.is_online() {
                    consecutive_failures = 0;
                }
                receive.set_offline(&ctx, &e);
                receive.set_online(false);
                eprintln!("[maild/receive] 账户 {}: {e}", cfg.account);
                let delay = reconnect_backoff(cfg.reconnect_secs, consecutive_failures);
                consecutive_failures = consecutive_failures.saturating_add(1);
                if !receive.backoff_sleep(delay).await {
                    return;
                }
            }
        }
    }
}

/// 断线重连的退避时长：`base * 2^min(failures, 6)`，封顶 300s，再加 ≤10% 的抖动
/// （ADR-0041 决策 3）。断网时不再每 5 秒空转一次 CPU 与钥匙串；多账户的重连也被错开。
fn reconnect_backoff(base_secs: u64, failures: u32) -> Duration {
    let exponent = failures.min(RECONNECT_BACKOFF_MAX_EXPONENT);
    let base = Duration::from_secs(base_secs.max(1));
    let capped = base
        .saturating_mul(1u32 << exponent)
        .min(RECONNECT_BACKOFF_MAX.max(base));
    let jitter_ceiling_ms = (capped.as_millis() / 10).max(1) as u64;
    capped + Duration::from_millis(pseudo_random() % jitter_ceiling_ms)
}

/// 退避抖动用的廉价伪随机数：不需要密码学强度，只需要多账户之间错开。
fn pseudo_random() -> u64 {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_ok() {
        return u64::from_le_bytes(bytes);
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.subsec_nanos() as u64)
        .unwrap_or(0)
}

/// RFC 2971 `ID`：网易（163 / 126 / yeah.net）等服务商要求客户端在 LOGIN 之后自报身份，
/// 否则接下来的命令会被以 "Unsafe Login" 拒绝，表现为账户永远连不上。
///
/// ID 是可选扩展：只在服务器宣告 CAPABILITY 时发送，失败也不终止会话——绝大多数服务商
/// 根本不需要它，不能让一个可选握手挡住正常连接。这里只报客户端名和版本，不带任何
/// 账户、地址或主机信息。
async fn identify_client(
    session: &mut async_imap::Session<Conn>,
    account: &str,
) {
    let supported = match session.capabilities().await {
        Ok(capabilities) => capabilities.has_str("ID"),
        Err(error) => {
            eprintln!("[maild/receive] 账户 {account}: 读取 CAPABILITY 失败（继续）: {error}");
            return;
        }
    };
    if !supported {
        return;
    }
    let identification = [
        ("name", Some(maild_brand::SLUG)),
        ("version", Some(env!("CARGO_PKG_VERSION"))),
    ];
    if let Err(error) = session.id(identification).await {
        eprintln!("[maild/receive] 账户 {account}: IMAP ID 被拒绝（继续）: {error}");
    }
}

/// 一次连接会话：登录 → SELECT → 抓取循环（IDLE 或轮询），直到断线或关断。
///
/// 秘密以短生命周期缓冲传入：认证完成后立即释放，不在会话期间长期驻留。
async fn serve(
    cfg: &ReceiveConfig,
    ctx: &Context,
    receive: &Receive,
    scheduler: Option<&Scheduler>,
    secret: SecretBuffer,
) -> Result<(), ReceiveError> {
    let client = connect_client(cfg).await?;
    let password = std::str::from_utf8(secret.expose()).map_err(fixed(
        AccountDetailCode::CredentialInvalid,
        "账户密码不是有效的 UTF-8 文本",
    ))?;
    // 休眠唤醒后的半开连接上，LOGIN 可能永远等不到应答（ADR-0041 决策 3）。
    let login_timeout = stage_timeouts().login;
    let mut session = tokio::time::timeout(login_timeout, client.login(&cfg.username, password))
        .await
        .map_err(fixed(AccountDetailCode::LoginTimeout, "IMAP 登录超时"))?
        .map_err(|(e, _)| ReceiveError::with_detail(AccountDetailCode::LoginFailed, "IMAP 登录失败", e))?;
    // 认证完成：立即释放秘密缓冲（协议库内部的一次性拷贝由其自身边界负责）。
    // password 是 secret 的借用，NLL 在此之后结束；drop(secret) 清零并释放缓冲。
    drop(secret);
    identify_client(&mut session, &cfg.account).await;

    let capabilities = read_capabilities(&mut session, &cfg.account).await;
    // LIST → 登记文件夹 → 决定本次会话的同步目标。主文件夹在 targets[0]。
    let mut targets = match discover_folders(&mut session, cfg, ctx).await {
        Ok(mut targets) if !targets.is_empty() => {
            targets.truncate(MAX_SYNCED_FOLDERS);
            targets
        }
        result => {
            if let Err(error) = result {
                eprintln!(
                    "[maild/receive] 账户 {}: LIST 失败，退回单信箱: {error}",
                    cfg.account
                );
            }
            vec![fallback_target(cfg)]
        }
    };

    // 上次会话停在 in_flight 的 op 与失联同处境：可重放的回到 pending，
    // 不可重放的落到 unknown 交用户裁决（ADR-0027 决策 2）。
    if let Some(store) = store_of(ctx) {
        if let Err(error) = store.recover_in_flight_ops(&cfg.account, now_unix()) {
            eprintln!("[maild/receive] 账户 {}: 收敛在途 op 失败: {error}", cfg.account);
        }
    }

    let mut sync_rx = receive.sync_rx();
    if receive.has_queued_manual_sync() {
        sync_rx.mark_changed();
    }
    receive.set_state(ctx, AccountStateKind::Online);
    receive.set_online(true);

    // 会话内每文件夹状态（游标 / 对账标记 / 有界去重环）：常驻内存 O(folders)，
    // 与挂机时长无关（ADR-0041 决策 2）。跨重启的去重由 (account, folder_id, uid) 唯一索引兜底。
    let mut sync = SyncSession {
        account: &cfg.account,
        ctx,
        capabilities,
        initial_sync_limit: cfg.initial_sync_limit,
        folders: HashMap::new(),
        selected: None,
    };
    let mut shutdown_rx = receive.shutdown_rx();
    let mut wake_rx = receive.wake_rx();
    // 接收端独占到本次连接结束；断线后请求方会收到 oneshot 断开而不是静默丢弃。
    let mut search_rx = receive.inner.search_rx.lock().await;
    let mut fetch_rx = receive.inner.fetch_rx.lock().await;
    // sweep 周期设下界，防配置成 0 把连接烧成忙循环。
    let sweep_interval = Duration::from_secs(cfg.folder_poll_secs.max(30));
    // 首次连接立刻扫一遍非主文件夹。
    let mut next_sweep = tokio::time::Instant::now();

    if cfg.idle {
        loop {
            if receive.is_shutdown() {
                let _ = session.logout().await;
                return Ok(());
            }
            sync.sync_folder(&mut session, &targets[0], true).await?;
            if tokio::time::Instant::now() >= next_sweep {
                sync.sweep_secondary_folders(&mut session, &targets).await?;
                next_sweep = tokio::time::Instant::now() + sweep_interval;
            }
            // 写队列在 IDLE 之间的间隙执行：读写因此天然串行，
            // IMAP 会话的选中状态不需要并发保护（ADR-0027 决策 8）。
            drain_pending_ops(&mut session, &cfg.account, ctx, &mut sync.selected, capabilities)
                .await?;
            // 清完队列可能已经离开主文件夹，IDLE 必须挂回去。
            ensure_selected(
                &mut session,
                &mut sync.selected,
                &targets[0].name,
                capabilities.condstore,
            )
            .await?;
            // 非 IDLE 期间服务器推来的 EXISTS 等会堆进 async-imap 的未请求响应通道
            // （容量 100、每条可能引用一整块读缓冲）。没人消费就会一直驻留，挂机越久越满。
            drain_unsolicited(&session);

            let mut idle = session.idle();
            tokio::time::timeout(stage_timeouts().idle_init, idle.init())
                .await
                .map_err(fixed(AccountDetailCode::IdleTimeout, "IDLE 启动超时"))?
                .map_err(stage(AccountDetailCode::IdleFailed, "IDLE 失败"))?;
            let (waiter, stop) = idle.wait();
            // sweep 到期要能把 IDLE 叫醒；续期点再按 RFC 2177 封顶 25 分钟。
            let until_sweep = next_sweep.saturating_duration_since(tokio::time::Instant::now());
            let idle_for = until_sweep
                .min(IDLE_RENEW_INTERVAL)
                .max(Duration::from_secs(1));
            tokio::select! {
                res = waiter => {
                    drop(stop);
                    if let Err(e) = res {
                        let _ = tokio::time::timeout(stage_timeouts().idle_done, idle.done()).await;
                        return Err(ReceiveError::with_detail(
                            AccountDetailCode::IdleFailed,
                            "IDLE 出错",
                            e,
                        ));
                    }
                    session = finish_idle(idle).await?;
                }
                _ = tokio::time::sleep(idle_for) => {
                    drop(stop);
                    session = finish_idle(idle).await?;
                }
                _ = wake_rx.changed() => {
                    drop(stop);
                    session = finish_idle(idle).await?;
                }
                _ = sync_rx.changed() => {
                    drop(stop);
                    session = finish_idle(idle).await?;
                    match sync.refresh_on_wake(&mut session, cfg, receive, &mut targets).await {
                        Some(true) => next_sweep = tokio::time::Instant::now() + sweep_interval,
                        Some(false) => {}
                        None => next_sweep = tokio::time::Instant::now(),
                    }
                }
                request = search_rx.recv() => {
                    drop(stop);
                    session = finish_idle(idle).await?;
                    if let Some(request) = request {
                        serve_server_search(
                            &mut session,
                            &cfg.account,
                            &mut sync.selected,
                            capabilities,
                            request,
                        )
                        .await;
                    }
                }
                request = fetch_rx.recv() => {
                    drop(stop);
                    session = finish_idle(idle).await?;
                    if let Some(request) = request {
                        sync.serve_fetch_by_uid(&mut session, &targets, request).await;
                    }
                }
                _ = shutdown_rx.changed() => {
                    drop(stop);
                    session = finish_idle(idle).await?;
                    let _ = session.logout().await;
                    return Ok(());
                }
            }
        }
    } else {
        // 每次连接先立即抓取一次，之后由共享 scheduler 产生固定延迟 tick。
        sync.sync_folder(&mut session, &targets[0], true).await?;
        drain_pending_ops(&mut session, &cfg.account, ctx, &mut sync.selected, capabilities)
            .await?;
        let scheduler = scheduler.ok_or_else(|| {
            ReceiveError::new(AccountDetailCode::PollFailed, "轮询 receive 缺少 scheduler 服务")
        })?;
        let (tick_tx, mut tick_rx) = mpsc::channel(1);
        let account = cfg.account.clone();
        let _poll_job = scheduler
            .schedule_account(
                account,
                ScheduleSpec::Interval {
                    every: Duration::from_secs(cfg.poll_secs.max(1)),
                    initial: InitialDelay::AfterPeriod,
                    jitter: JitterPolicy::Default,
                },
                move |_| {
                    let tick_tx = tick_tx.clone();
                    Box::pin(async move {
                        match tick_tx.try_send(()) {
                            Ok(()) | Err(mpsc::error::TrySendError::Full(())) => Ok(()),
                            Err(mpsc::error::TrySendError::Closed(())) => {
                                Err(ScheduleError::Cancelled)
                            }
                        }
                    })
                },
            )
            .map_err(|error| {
                ReceiveError::new(AccountDetailCode::PollFailed, format!("注册轮询任务失败: {error}"))
            })?;

        loop {
            drain_unsolicited(&session);
            // 轮询路径的一轮完整同步：主文件夹 → 到期则 sweep → 清写队列。
            let full_round = tokio::select! {
                tick = tick_rx.recv() => {
                    if tick.is_none() {
                        return Err(ReceiveError::new(
                            AccountDetailCode::PollFailed,
                            "轮询 scheduler 通道已关闭",
                        ));
                    }
                    true
                }
                _ = sync_rx.changed() => {
                    match sync.refresh_on_wake(&mut session, cfg, receive, &mut targets).await {
                        Some(all) => {
                            if all {
                                next_sweep = tokio::time::Instant::now() + sweep_interval;
                            }
                            false
                        }
                        None => {
                            next_sweep = tokio::time::Instant::now();
                            true
                        }
                    }
                }
                _ = wake_rx.changed() => {
                    drain_pending_ops(
                        &mut session,
                        &cfg.account,
                        ctx,
                        &mut sync.selected,
                        capabilities,
                    )
                    .await?;
                    false
                }
                request = search_rx.recv() => {
                    if let Some(request) = request {
                        serve_server_search(
                            &mut session,
                            &cfg.account,
                            &mut sync.selected,
                            capabilities,
                            request,
                        )
                        .await;
                    }
                    false
                }
                request = fetch_rx.recv() => {
                    if let Some(request) = request {
                        sync.serve_fetch_by_uid(&mut session, &targets, request).await;
                    }
                    false
                }
                _ = shutdown_rx.changed() => {
                    let _ = session.logout().await;
                    return Ok(());
                }
            };
            if !full_round {
                continue;
            }
            sync.sync_folder(&mut session, &targets[0], true).await?;
            if tokio::time::Instant::now() >= next_sweep {
                sync.sweep_secondary_folders(&mut session, &targets).await?;
                next_sweep = tokio::time::Instant::now() + sweep_interval;
            }
            drain_pending_ops(&mut session, &cfg.account, ctx, &mut sync.selected, capabilities)
                .await?;
        }
    }
}

/// 结束一次 IDLE 并取回会话。`DONE` 之后等不到 tagged 应答 = 连接已死（ADR-0041 决策 3）。
async fn finish_idle(
    idle: async_imap::extensions::idle::Handle<Conn>,
) -> Result<async_imap::Session<Conn>, ReceiveError> {
    tokio::time::timeout(stage_timeouts().idle_done, idle.done())
        .await
        .map_err(fixed(AccountDetailCode::IdleTimeout, "IDLE done 超时（连接可能已失效）"))?
        .map_err(stage(AccountDetailCode::IdleFailed, "IDLE done 失败"))
}

/// 丢弃积压的未请求响应（只为释放内存；新邮件由 IDLE / 增量同步发现）。
fn drain_unsolicited(session: &async_imap::Session<Conn>) {
    while session.unsolicited_responses.try_recv().is_ok() {}
}

/// IMAP 引用串：`\` 与 `"` 转义后加双引号。
fn imap_quoted(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// 在当前连接上执行一次服务器检索并回传结果（ADR-0029 决策 11）。
///
/// **不用 `?`**：一次搜索失败不该把收信连接一起带走。错误原样回给请求方，
/// 连接继续；真正的连接故障会在下一次同步时暴露。
async fn serve_server_search(
    session: &mut async_imap::Session<Conn>,
    account: &str,
    selected: &mut Selected,
    capabilities: ServerCapabilities,
    request: ServerSearchRequest,
) {
    let outcome = run_server_search(
        session,
        account,
        selected,
        capabilities,
        &request.folder,
        &request.query,
        request.since,
        request.until,
    )
    .await;
    let _ = request.reply.send(outcome);
}

#[allow(clippy::too_many_arguments)]
async fn run_server_search(
    session: &mut async_imap::Session<Conn>,
    account: &str,
    selected: &mut Selected,
    capabilities: ServerCapabilities,
    folder: &str,
    query: &str,
    since: Option<u64>,
    until: Option<u64>,
) -> Result<Vec<MessageSummary>, String> {
    // 服务器检索是 RPC，报错仍是协议 `error.message`（ADR-0048 L6 不涉及）。
    ensure_selected(session, selected, folder, capabilities.condstore)
        .await
        .map_err(|error| error.to_string())?;
    // 非 ASCII 查询必须声明字符集。RFC 3501 严格来说要求用 literal 传 8bit，
    // 但那需要续行握手；这里用 `CHARSET UTF-8` + 引用串，服务器不接受时回 BAD，
    // 原样透出给用户，而不是假装搜过了（ADR-0029 决策 11）。
    let criteria = if query.is_ascii() {
        format!("TEXT {}", imap_quoted(query))
    } else {
        format!("CHARSET UTF-8 TEXT {}", imap_quoted(query))
    };
    let hits = session
        .uid_search(criteria)
        .await
        .map_err(|error| format!("服务器检索失败: {error}"))?;
    let mut uids: Vec<u32> = hits.into_iter().collect();
    // 最近的在前，并且有上限——服务器可能命中整个信箱。
    uids.sort_unstable_by(|left, right| right.cmp(left));
    uids.truncate(crate::search::MAX_SERVER_SEARCH_HITS);
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let fetches = session
        .uid_fetch(set, "(UID FLAGS INTERNALDATE BODY.PEEK[HEADER])")
        .await
        .map_err(|error| format!("服务器检索取头失败: {error}"))?;
    futures::pin_mut!(fetches);
    let now = now_unix();
    let max_arrival = now.saturating_add(mime::DATE_FUTURE_TOLERANCE_SECS);
    let mut out: Vec<MessageSummary> = Vec::new();
    while let Some(fetch) = fetches.next().await {
        let fetch = fetch.map_err(|error| format!("服务器检索取头出错: {error}"))?;
        let Some(uid) = fetch.uid else { continue };
        let header = fetch.header().unwrap_or_default();
        let received_at = fetch
            .internal_date()
            .and_then(|dt| u64::try_from(dt.timestamp()).ok())
            .filter(|ts| (mime::EARLIEST_PLAUSIBLE_DATE_SECS..=max_arrival).contains(ts))
            .unwrap_or(0);
        let mut message = parse_message_at(account, &uid.to_string(), header, received_at);
        message.folder = folder.to_string();
        apply_flags(&mut message, &fetch);
        // 有可信 INTERNALDATE 时按标准 sort_key 锚定；服务器省去 INTERNALDATE 时仍挡住远未来伪造 Date。
        message.sort_at = if received_at > 0 {
            mime::sort_key(message.date_at, received_at)
        } else {
            message
                .date_at
                .filter(|date| *date <= max_arrival)
                .unwrap_or(0)
        };
        let summary = MessageSummary::from(&message);
        if since.is_some_and(|bound| summary.sort_at < bound)
            || until.is_some_and(|bound| summary.sort_at > bound)
        {
            continue;
        }
        out.push(summary);
    }
    out.sort_by_key(|summary| std::cmp::Reverse(summary.sort_at));
    Ok(out)
}

/// 与服务器对账标记位，并（在全量轮次）发现已被其他客户端 `EXPUNGE` 的消息。
async fn reconcile_folder_flags(
    session: &mut async_imap::Session<Conn>,
    target: &SyncTarget,
    folder_id: i64,
    account: &str,
    ctx: &Context,
    selected: &Selected,
    capabilities: ServerCapabilities,
) -> Result<(), ReceiveError> {
    let server_modseq = selected
        .as_ref()
        .filter(|current| current.name == target.name)
        .and_then(|current| current.highest_modseq);
    let stored_modseq = match store_of(ctx) {
        Some(store) => store
            .folder_modseq(folder_id)
            .map_err(store_stage("读取 modseq 失败"))?,
        None => return Ok(()),
    };

    // CONDSTORE 短路：HIGHESTMODSEQ 未变 = 自上次同步以来没有任何标记变化。
    if capabilities.condstore && server_modseq.is_some() && server_modseq == stored_modseq {
        return Ok(());
    }
    let changed_since = capabilities.condstore.then_some(stored_modseq).flatten();
    let query = match changed_since {
        Some(modseq) => format!("(UID FLAGS) (CHANGEDSINCE {modseq})"),
        None => "(UID FLAGS)".to_string(),
    };

    let mut observed: Vec<(u32, ObservedFlags)> = Vec::new();
    {
        let fetches = session
            .uid_fetch("1:*", query)
            .await
            .map_err(sync_stage("对账 FETCH 失败"))?;
        futures::pin_mut!(fetches);
        while let Some(fetch) = fetches.next().await {
            let fetch = fetch.map_err(sync_stage("对账 FETCH 流出错"))?;
            let Some(uid) = fetch.uid else { continue };
            let mut flags = ObservedFlags::default();
            for flag in fetch.flags() {
                match flag {
                    async_imap::types::Flag::Seen => flags.seen = true,
                    async_imap::types::Flag::Flagged => flags.flagged = true,
                    async_imap::types::Flag::Answered => flags.answered = true,
                    async_imap::types::Flag::Deleted => flags.deleted = true,
                    _ => {}
                }
            }
            observed.push((uid, flags));
        }
    }

    // 只有拿到了完整 UID 集合的轮次才做存在性对账。
    let full = changed_since.is_none();
    let protect_after = now_unix().saturating_sub(RECONCILE_SKIP_SECS);
    let Some(store) = store_of(ctx) else {
        return Ok(());
    };
    let outcome = store
        .reconcile_flags(folder_id, &observed, full, protect_after)
        .map_err(store_stage("对账写回失败"))?;
    store
        .set_folder_modseq(folder_id, server_modseq)
        .map_err(store_stage("写入 modseq 失败"))?;
    if outcome.updated > 0 || outcome.removed > 0 {
        eprintln!(
            "[maild/receive] 账户 {account}: 文件夹 {} 对账 更新 {} / 移除 {} / 保护 {}",
            target.display_name, outcome.updated, outcome.removed, outcome.protected
        );
    }
    Ok(())
}

/// 执行队列里的待办，直到清空。
///
/// 连接类错误立刻中止整轮并向上传播——会话已经不可用，剩下的 op 留给重连后的下一轮
/// （它们会被 `recover_in_flight_ops` 收敛，ADR-0027 决策 2）。
async fn drain_pending_ops(
    session: &mut async_imap::Session<Conn>,
    account: &str,
    ctx: &Context,
    selected: &mut Selected,
    capabilities: ServerCapabilities,
) -> Result<(), ReceiveError> {
    loop {
        let Some(store) = store_of(ctx) else {
            return Ok(());
        };
        let claimed = store
            .claim_next_op(account, now_unix())
            .map_err(store_stage("取出待办失败"))?;
        drop(store);
        let Some(op) = claimed else { return Ok(()) };

        if op.folder.is_empty() {
            settle_op(ctx, &op, OpState::Failed, "文件夹记录已丢失");
            continue;
        }
        let (outcome, detail, fatal) = execute_op(session, &op, selected, capabilities).await;
        settle_op(ctx, &op, outcome, &detail);
        if let Some(error) = fatal {
            return Err(error);
        }
    }
}

// —— 写队列命令失败说明的前缀（ADR-0048 L6）——
// `listPendingOps` 摘要按同一张表拆出服务器原文（`detail_code = "server_rejected"`），两边不能漂移。
// `SELECT` 的前缀带文件夹名，见 [`op_select_prefix`]。
const OP_APPEND_FAILED: &str = "APPEND 失败";
const OP_STORE_FAILED: &str = "STORE 失败";
const OP_MOVE_FAILED: &str = "MOVE 失败";
const OP_COPY_FAILED: &str = "COPY 失败";
const OP_COPY_UNMARKED: &str = "COPY 已成功但标记删除失败";
const OP_EXPUNGE_FAILED: &str = "已复制并标记删除，EXPUNGE 失败";
const OP_FAILURE_PREFIXES: [&str; 6] = [
    OP_APPEND_FAILED,
    OP_STORE_FAILED,
    OP_MOVE_FAILED,
    OP_COPY_FAILED,
    OP_COPY_UNMARKED,
    OP_EXPUNGE_FAILED,
];

fn op_select_prefix(folder: &str) -> String {
    format!("SELECT {folder} 失败")
}

/// 写队列 op 失败说明里服务器给的原文（`前缀: 原文` 的后半段）。说明不是服务器命令失败
/// （用户放弃、载荷无法解析、文件夹记录丢失……）时为 `None`。老版本落库的说明同形，同样适用。
pub(crate) fn op_failure_server_text<'a>(folder: &str, detail: &'a str) -> Option<&'a str> {
    let select = op_select_prefix(folder);
    OP_FAILURE_PREFIXES
        .iter()
        .copied()
        .chain(std::iter::once(select.as_str()))
        .find_map(|prefix| detail.strip_prefix(prefix)?.strip_prefix(": "))
}

/// 一次写队列命令失败：按 IMAP 错误决定终态与是否连接级故障（ADR-0027 决策 2）。
/// 说明是 `前缀: 原文`；连接级故障另返回同一句的错误，让收信循环带着 code 掉线。
fn op_failure(
    prefix: impl std::fmt::Display,
    error: &async_imap::error::Error,
) -> (OpState, String, Option<ReceiveError>) {
    let (state, fatal) = classify_op_error(error);
    let failure = ReceiveError::with_detail(AccountDetailCode::SyncFailed, prefix, error);
    (state, failure.to_string(), fatal.then_some(failure))
}

fn settle_op(ctx: &Context, op: &PendingOp, outcome: OpState, detail: &str) {
    let Some(store) = store_of(ctx) else { return };
    match store.finish_op(op, outcome, detail, now_unix()) {
        Ok(settled) => {
            if settled != OpState::Done {
                eprintln!(
                    "[maild/receive] 账户 {}: op #{} ({}) → {}: {detail}",
                    op.account,
                    op.id,
                    op.kind.as_str(),
                    settled.as_str()
                );
            }
        }
        Err(error) => eprintln!("[maild/receive] 落定 op #{} 失败: {error}", op.id),
    }
}

/// 返回 (终态, 详情, 连接级故障)。连接级故障为 `Some` 时整轮中止、收信循环掉线。
async fn execute_op(
    session: &mut async_imap::Session<Conn>,
    op: &PendingOp,
    selected: &mut Selected,
    capabilities: ServerCapabilities,
) -> (OpState, String, Option<ReceiveError>) {
    let done = || (OpState::Done, String::new(), None);
    if op.kind == OpKind::Append {
        // APPEND 的实参就是 mailbox 名，不需要先选中它。
        return match session.append(&op.folder, op.payload.as_bytes()).await {
            Ok(()) => done(),
            Err(error) => op_failure(OP_APPEND_FAILED, &error),
        };
    }
    if let Err(error) = select_mailbox(session, selected, &op.folder, capabilities.condstore).await
    {
        // 服务器明确拒绝（文件夹不存在）= 命令根本没发出，是 failed 而不是未知——
        // 消息应该回到源文件夹，而不是留在「结果未知」的限台上等用户裁决。
        return op_failure(op_select_prefix(&op.folder), &error);
    }
    match op.kind {
        // 上面已提前返回。
        OpKind::Append => (OpState::Failed, "不可达".into(), None),
        OpKind::Flag => {
            let Some(payload) = op.flag_payload() else {
                return (OpState::Failed, "flag 载荷无法解析".into(), None);
            };
            let sign = if payload.value { "+" } else { "-" };
            let query = format!("{sign}FLAGS.SILENT ({})", payload.flag.imap_name());
            match drain_store(session, &op.uid, &query).await {
                Ok(()) => done(),
                Err(error) => op_failure(OP_STORE_FAILED, &error),
            }
        }
        OpKind::Move => {
            let Some(payload) = op.move_payload() else {
                return (OpState::Failed, "move 载荷无法解析".into(), None);
            };
            if capabilities.move_command {
                return match session.uid_mv(&op.uid, &payload.target).await {
                    Ok(()) => done(),
                    Err(error) => op_failure(OP_MOVE_FAILED, &error),
                };
            }
            // 降级链：COPY → +\Deleted →（仅在 UIDPLUS 下）UID EXPUNGE。
            if let Err(error) = session.uid_copy(&op.uid, &payload.target).await {
                return op_failure(OP_COPY_FAILED, &error);
            }
            if let Err(error) = drain_store(session, &op.uid, "+FLAGS.SILENT (\\Deleted)").await {
                let (state, detail, fatal) = op_failure(OP_COPY_UNMARKED, &error);
                // COPY 已经成功，源件是否清理未知——不能报成 failed（会把消息放回来）。
                let state = if state == OpState::Failed {
                    OpState::Unknown
                } else {
                    state
                };
                return (state, detail, fatal);
            }
            if !capabilities.uidplus {
                // 裸 EXPUNGE 会连带清除其他客户端标记 \Deleted 的邮件——那是别人的数据。
                return (
                    OpState::Done,
                    "服务器无 UIDPLUS：已复制并标记删除，未 EXPUNGE".into(),
                    None,
                );
            }
            match drain_expunge(session, &op.uid).await {
                Ok(()) => done(),
                Err(error) => {
                    // 同上：COPY 已成功，源件残留与否不改变「移动已完成」这个事实。
                    let (_, detail, fatal) = op_failure(OP_EXPUNGE_FAILED, &error);
                    (OpState::Done, detail, fatal)
                }
            }
        }
    }
}

async fn drain_store(
    session: &mut async_imap::Session<Conn>,
    uid: &str,
    query: &str,
) -> Result<(), async_imap::error::Error> {
    let updates = session.uid_store(uid, query).await?;
    futures::pin_mut!(updates);
    while let Some(update) = updates.next().await {
        update?;
    }
    Ok(())
}

async fn drain_expunge(
    session: &mut async_imap::Session<Conn>,
    uid: &str,
) -> Result<(), async_imap::error::Error> {
    let expunged = session.uid_expunge(uid).await?;
    futures::pin_mut!(expunged);
    while let Some(item) = expunged.next().await {
        item?;
    }
    Ok(())
}

/// 按 `tls` 模式建立连接：
/// - none：明文
/// - starttls：手动协商（greeting → `a0 STARTTLS` → OK → 就地 TLS 握手）
/// - implicit：直接 TLS 握手（993）
async fn connect_client(cfg: &ReceiveConfig) -> Result<Client<Conn>, ReceiveError> {
    let stream = connect_tcp(&cfg.server, cfg.port).await?;
    // 握手阶段同样可能撞上半开连接：整体设上限（ADR-0041 决策 3）。
    tokio::time::timeout(stage_timeouts().handshake, negotiate(cfg, stream))
        .await
        .map_err(fixed(AccountDetailCode::TlsTimeout, "TLS / STARTTLS 握手超时"))?
}

/// 解析全部地址并逐个尝试：单地址 10s、总计 20s（ADR-0041 决策 3）。
///
/// 双栈环境里第一个地址（常见是 IPv6）不通时，`TcpStream::connect` 会在它上面耗满
/// 系统默认的 75s；这里给每个地址独立预算，后面的地址还有机会。
async fn connect_tcp(server: &str, port: u16) -> Result<TcpStream, ReceiveError> {
    let timeouts = stage_timeouts();
    let prefix = format!("连接 {server}:{port} 失败");
    // 超时与「没有地址」是我们的判断（`connect_timeout` / `connect_failed`，无原文）；
    // DNS 与套接字报错是系统原文。
    let ours = |code, reason: String| ReceiveError::new(code, format!("{prefix}: {reason}"));
    let deadline = tokio::time::Instant::now() + timeouts.connect_total;
    let addrs: Vec<std::net::SocketAddr> =
        tokio::time::timeout(timeouts.connect_total, tokio::net::lookup_host((server, port)))
            .await
            .map_err(|_| ours(AccountDetailCode::ConnectTimeout, "DNS 解析超时".into()))?
            .map_err(stage(AccountDetailCode::ConnectFailed, &prefix))?
            .collect();
    let mut last_error = ours(AccountDetailCode::ConnectFailed, "没有可用地址".into());
    for addr in addrs {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            last_error = ours(AccountDetailCode::ConnectTimeout, "连接超时".into());
            break;
        }
        match tokio::time::timeout(timeouts.connect_per_addr.min(remaining), TcpStream::connect(addr))
            .await
        {
            Ok(Ok(stream)) => {
                tune_socket(&stream);
                return Ok(stream);
            }
            Ok(Err(error)) => {
                last_error = ReceiveError::with_detail(AccountDetailCode::ConnectFailed, &prefix, error)
            }
            Err(_) => {
                last_error = ours(AccountDetailCode::ConnectTimeout, format!("{addr} 连接超时"))
            }
        }
    }
    Err(last_error)
}

/// 关 Nagle（IMAP 是小命令往返），开低开销 TCP keepalive：笔电盒盖休眠后，半开连接会被
/// 内核探测出来并以错误返回，而不是让 IDLE 永远挂着（ADR-0041 决策 3）。尽力而为。
fn tune_socket(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = stream.as_raw_fd();
        let set = |level: libc::c_int, name: libc::c_int, value: libc::c_int| {
            // SAFETY: fd 在 stream 存活期间有效；value 是栈上的 c_int，长度如实传入。
            unsafe {
                libc::setsockopt(
                    fd,
                    level,
                    name,
                    &value as *const libc::c_int as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        };
        set(libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        set(libc::IPPROTO_TCP, libc::TCP_KEEPALIVE, TCP_KEEPALIVE_IDLE_SECS);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        set(libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, TCP_KEEPALIVE_IDLE_SECS);
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux", target_os = "android"))]
        {
            set(libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 30);
            set(libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 4);
        }
    }
}

async fn negotiate(cfg: &ReceiveConfig, stream: TcpStream) -> Result<Client<Conn>, ReceiveError> {
    match cfg.tls.as_str() {
        "implicit" => {
            let conn = build_tls_connector(cfg)?;
            let tls = conn
                .connect(&cfg.server, stream)
                .await
                .map_err(stage(AccountDetailCode::TlsFailed, "TLS 握手失败"))?;
            Ok(Client::new(Box::new(tls) as Conn))
        }
        "starttls" => {
            // RFC 2595 §2.2：逐字节读行——OK 之后的 TLS ClientHello 字节
            // 必须原样交给 TLS 层，不能用缓冲读。
            let mut stream = stream;
            let greeting = read_line_exact(&mut stream)
                .await
                .map_err(line_stage(AccountDetailCode::GreetingInvalid, "读 greeting 失败"))?;
            if !greeting.starts_with("* OK") && !greeting.starts_with("* PREAUTH") {
                return Err(ReceiveError::with_detail(
                    AccountDetailCode::GreetingInvalid,
                    "IMAP greeting 异常",
                    greeting,
                ));
            }
            stream
                .write_all(b"a0 STARTTLS\r\n")
                .await
                .map_err(stage(AccountDetailCode::TlsFailed, "写 STARTTLS 失败"))?;
            let resp = read_line_exact(&mut stream)
                .await
                .map_err(line_stage(AccountDetailCode::TlsFailed, "读 STARTTLS 响应失败"))?;
            if !resp.starts_with("a0 OK") {
                return Err(ReceiveError::with_detail(
                    AccountDetailCode::StarttlsUnsupported,
                    "服务器不支持 STARTTLS",
                    resp,
                ));
            }
            let conn = build_tls_connector(cfg)?;
            let tls = conn
                .connect(&cfg.server, stream)
                .await
                .map_err(stage(AccountDetailCode::TlsFailed, "TLS 握手失败"))?;
            Ok(Client::new(Box::new(tls) as Conn))
        }
        _ => Ok(Client::new(Box::new(stream) as Conn)),
    }
}

fn build_tls_connector(cfg: &ReceiveConfig) -> Result<tokio_native_tls::TlsConnector, ReceiveError> {
    let mut b = native_tls::TlsConnector::builder();
    // legacy 配置可显式携带自签开关；受管路径恒为 false，
    // 只有 debug 构建下的 hermetic 环境变量能打开豁免。
    if cfg.accept_invalid_certs || crate::credential::test_accept_invalid_certs() {
        b.danger_accept_invalid_certs(true);
    }
    let ntls = b
        .build()
        .map_err(stage(AccountDetailCode::TlsFailed, "构建 TLS connector 失败"))?;
    Ok(tokio_native_tls::TlsConnector::from(ntls))
}

/// 逐字节读一行（到 `\n` 为止，剥离 `\r`）。STARTTLS 协商专用：
/// 不预读任何多余字节（不缓冲），保证 TLS 握手字节完整。
pub(crate) async fn read_line_exact(stream: &mut TcpStream) -> Result<String, tokio::io::Error> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Err(tokio::io::Error::new(
                tokio::io::ErrorKind::UnexpectedEof,
                "连接在行尾前关闭",
            ));
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&line)
        .trim_end_matches('\r')
        .to_string())
}

/// 同步的文件夹数硬上限：防一个病态账户（或恶意服务器）用几千个 mailbox 拖垮 sweep。
const MAX_SYNCED_FOLDERS: usize = 64;

/// 每次操作现取 store，**绝不跨 await 持有强引用**。
///
/// 受管账户的停止是尽力而为的：`AccountManager::stop_runtime` 等待 `RUNTIME_STOP_TIMEOUT`
/// 后就放弃 join，任务可能继续存活（设计如此——一个卡住的服务器不该挡住守护进程退出）。
/// 如果收信循环长期握着 `Arc<Store>`，数据库锁就被扣在一个已经被放弃的任务里，
/// 下一次启动会打不开库。所以这里只在单次同步操作的范围内取用。
fn store_of(ctx: &Context) -> Option<Arc<Store>> {
    ctx.get::<Store>()
}

/// 一个同步目标。`folder_id` 缺席表示本次装配没有 store——退化为会话内游标，
/// 行为与 v4 之前的单信箱路径一致。
struct SyncTarget {
    /// 服务器线上原名（`SELECT` 的实参）。
    name: String,
    display_name: String,
    role: Option<FolderRole>,
    folder_id: Option<i64>,
}

/// 当前已选中的 mailbox 及其 `UIDVALIDITY` / `HIGHESTMODSEQ`。
///
/// RFC 3501 保证 `UIDVALIDITY` 在会话内对同一 mailbox 恒定，因此缓存它是安全的，
/// 且能省掉每次 IDLE 唤醒后的一次多余 `SELECT`。
///
/// 一律用 `SELECT` 而不是 `EXAMINE`：写路径（ADR-0027）需要可写会话，`CONDSTORE` 的
/// `HIGHESTMODSEQ` 也只在 `SELECT` 上取得到。正文一律 `BODY.PEEK`，不会隐式置 `\Seen`。
#[derive(Clone, Debug)]
struct SelectedMailbox {
    name: String,
    uid_validity: u32,
    highest_modseq: Option<u64>,
    /// SELECT 时的 EXISTS（冷启动窗口判定用）。
    exists: u32,
    /// SELECT 时的 UIDNEXT。只在刚 SELECT 完的那一刻可信，由 `sync_folder` 取走即作废
    /// （ADR-0041 决策 1）。
    uid_next: Option<u32>,
}

type Selected = Option<SelectedMailbox>;

/// 本次会话读到的服务器能力（ADR-0027 决策 3/7）。
#[derive(Clone, Copy, Debug, Default)]
struct ServerCapabilities {
    move_command: bool,
    uidplus: bool,
    condstore: bool,
}

/// 对账只跳过「最近这么久」内的本地乐观改写（ADR-0027 决策 6）。
const RECONCILE_SKIP_SECS: u64 = 60;

async fn read_capabilities(
    session: &mut async_imap::Session<Conn>,
    account: &str,
) -> ServerCapabilities {
    match session.capabilities().await {
        Ok(capabilities) => ServerCapabilities {
            move_command: capabilities.has_str("MOVE"),
            uidplus: capabilities.has_str("UIDPLUS"),
            condstore: capabilities.has_str("CONDSTORE"),
        },
        Err(error) => {
            eprintln!("[maild/receive] 账户 {account}: 读取 CAPABILITY 失败（按最小能力处理）: {error}");
            ServerCapabilities::default()
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

/// 确保目标 mailbox 已选中；已经选中则不再重发 `SELECT`。
///
/// 保留 IMAP 错误类型：写路径要靠它区分「文件夹不存在」（服务器明确拒绝）与
/// 「连接断了」（结果未知），两者对 op 的后果完全不同。
async fn select_mailbox(
    session: &mut async_imap::Session<Conn>,
    selected: &mut Selected,
    name: &str,
    condstore: bool,
) -> Result<(), async_imap::error::Error> {
    if selected.as_ref().is_some_and(|current| current.name == name) {
        return Ok(());
    }
    *selected = None;
    let mailbox = if condstore {
        session.select_condstore(name).await?
    } else {
        session.select(name).await?
    };
    *selected = Some(SelectedMailbox {
        name: name.to_string(),
        uid_validity: mailbox.uid_validity.unwrap_or(0),
        highest_modseq: mailbox.highest_modseq,
        exists: mailbox.exists,
        uid_next: mailbox.uid_next,
    });
    Ok(())
}

async fn ensure_selected(
    session: &mut async_imap::Session<Conn>,
    selected: &mut Selected,
    name: &str,
    condstore: bool,
) -> Result<(), ReceiveError> {
    select_mailbox(session, selected, name, condstore)
        .await
        .map_err(sync_stage(format!("SELECT {name} 失败")))
}

/// IMAP 错误 → op 终态（ADR-0027 决策 2）。
///
/// `NO`/`BAD`/`VALIDATE` 是服务器**明确拒绝**，语义确定；`IO`/`ConnectionLost`/`Parse`
/// 意味着我们没看到 tagged 响应——命令是否送达未知。
fn classify_op_error(error: &async_imap::error::Error) -> (crate::pending::OpState, bool) {
    use async_imap::error::Error;
    match error {
        Error::No(_) | Error::Bad(_) | Error::Validate(_) => (crate::pending::OpState::Failed, false),
        _ => (crate::pending::OpState::Unknown, true),
    }
}

/// RFC 6154 特殊用途属性 → `\Xxx` 字符串。判定逻辑在 `folders` 模块里，与网络层解耦。
fn special_use_token(attribute: &async_imap::types::NameAttribute<'_>) -> Option<String> {
    use async_imap::types::NameAttribute;
    Some(match attribute {
        NameAttribute::All => "\\All",
        NameAttribute::Archive => "\\Archive",
        NameAttribute::Drafts => "\\Drafts",
        NameAttribute::Junk => "\\Junk",
        NameAttribute::Sent => "\\Sent",
        NameAttribute::Trash => "\\Trash",
        _ => return None,
    }
    .to_string())
}

/// `LIST "" "*"` → 登记文件夹 → 返回本次会话要同步的目标（主文件夹在最前）。
///
/// 连接启动失败时退回配置里的 mailbox；手动刷新失败时保留已有目标。
fn fallback_target(cfg: &ReceiveConfig) -> SyncTarget {
    SyncTarget {
        name: cfg.mailbox.clone(),
        display_name: folders::decode_modified_utf7(&cfg.mailbox),
        role: Some(FolderRole::Inbox),
        folder_id: None,
    }
}

async fn discover_folders(
    session: &mut async_imap::Session<Conn>,
    cfg: &ReceiveConfig,
    ctx: &Context,
) -> Result<Vec<SyncTarget>, ReceiveError> {
    if store_of(ctx).is_none() {
        return Ok(vec![fallback_target(cfg)]);
    }

    let listed = tokio::time::timeout(Duration::from_secs(30), list_mailboxes(session))
        .await
        .map_err(fixed(AccountDetailCode::SyncFailed, "LIST 超时"))??;
    if !listed
        .iter()
        .any(|entry| entry.selectable && entry.name.eq_ignore_ascii_case("INBOX"))
    {
        return Err(ReceiveError::new(
            AccountDetailCode::SyncFailed,
            "LIST 未返回可选的 INBOX",
        ));
    }

    let present: Vec<String> = listed.iter().map(|entry| entry.name.clone()).collect();
    let Some(store) = store_of(ctx) else {
        return Ok(vec![fallback_target(cfg)]);
    };
    store
        .mark_absent_folders(&cfg.account, &present)
        .map_err(store_stage("标记缺席文件夹失败"))?;

    let mut targets = Vec::new();
    for entry in listed {
        let folder_id = store
            .upsert_folder(&cfg.account, &entry.name, &entry.display_name, entry.role)
            .map_err(store_stage("登记文件夹失败"))?;
        // `\NoSelect` 是纯层级节点：登记但不同步。
        if !entry.selectable {
            continue;
        }
        targets.push(SyncTarget {
            name: entry.name,
            display_name: entry.display_name,
            role: entry.role,
            folder_id: Some(folder_id),
        });
    }
    drop(store);
    if targets.is_empty() {
        return Ok(targets);
    }

    // 主文件夹：配置指定的 mailbox 优先，其次 role=inbox，最后第一个。
    let primary = targets
        .iter()
        .position(|target| target.name.eq_ignore_ascii_case(&cfg.mailbox))
        .or_else(|| targets.iter().position(|t| t.role == Some(FolderRole::Inbox)))
        .unwrap_or(0);
    targets.swap(0, primary);
    Ok(targets)
}

struct ListedFolder {
    name: String,
    display_name: String,
    role: Option<FolderRole>,
    selectable: bool,
}

/// async-imap 的 LIST 流丢弃 tagged 状态，需读到对应的 OK 才能使用结果。
async fn list_mailboxes(
    session: &mut async_imap::Session<Conn>,
) -> Result<Vec<ListedFolder>, ReceiveError> {
    use async_imap::imap_proto::{MailboxDatum, Response, Status};
    use async_imap::types::NameAttribute;

    let request = session
        .run_command(r#"LIST "" "*""#)
        .await
        .map_err(sync_stage("LIST 失败"))?;
    let mut out = Vec::new();
    while let Some(response) = session.read_response().await {
        let response = response.map_err(sync_stage("LIST 流出错"))?;
        match response.parsed() {
            Response::Done {
                tag,
                status,
                information,
                ..
            } if tag == &request => {
                if *status == Status::Ok {
                    return Ok(out);
                }
                return Err(sync_stage("LIST 失败")(format_args!(
                    "{status:?} {}",
                    information.as_deref().unwrap_or_default()
                )));
            }
            Response::Data {
                status: Status::Bye,
                information,
                ..
            } => {
                return Err(sync_stage("LIST 连接已断开")(
                    information.as_deref().unwrap_or_default(),
                ));
            }
            Response::MailboxData(MailboxDatum::List {
                name,
                name_attributes,
                delimiter,
            }) if !name.is_empty() => {
                let attributes: Vec<String> = name_attributes
                    .iter()
                    .filter_map(special_use_token)
                    .collect();
                let display_name = folders::decode_modified_utf7(name);
                let role =
                    folders::resolve_role(&attributes, name, &display_name, delimiter.as_deref());
                out.push(ListedFolder {
                    name: name.to_string(),
                    display_name,
                    role,
                    selectable: !name_attributes.contains(&NameAttribute::NoSelect),
                });
            }
            _ => {}
        }
    }
    Err(ReceiveError::new(
        AccountDetailCode::SyncFailed,
        "LIST 未收到命令完成响应",
    ))
}

/// 把 IMAP 标记位快照到消息上。
///
/// 只在**首次抓取**时记录：服务器改标记不改 UID，UID 游标看不见它。
/// 全量对账归 M4.5-05（ADR-0026 非目标）。
fn apply_flags(message: &mut MailMessage, fetch: &async_imap::types::Fetch) {
    use async_imap::types::Flag;
    for flag in fetch.flags() {
        match flag {
            Flag::Seen => message.seen = true,
            Flag::Flagged => message.flagged = true,
            Flag::Answered => message.answered = true,
            Flag::Deleted => message.deleted = true,
            _ => {}
        }
    }
}

/// 会话内某个文件夹的同步状态。全部字段定长，常驻内存与挂机时长无关（ADR-0041 决策 2）。
#[derive(Debug, Default)]
struct FolderSession {
    /// 无 store 装配时的会话内游标（有 store 时游标持久化在 folders 表）。
    cursor: Option<u32>,
    /// 本次会话是否已做过标记对账（对账是 O(mailbox)，每会话一次）。
    reconciled: bool,
    recent: RecentUids,
}

/// 有界去重：最高已处理 UID + 最近 [`SESSION_DEDUP_CAPACITY`] 个 UID 的环。
///
/// 只负责吸收 IDLE 唤醒重叠导致的重复推送。新邮件的 UID 严格递增（RFC 3501 §2.3.1.1），
/// 所以被挤出环的旧 UID 一律视为已处理——`floor` 记住被挤出的最大值即可。
#[derive(Debug, Default)]
struct RecentUids {
    highest: u32,
    floor: u32,
    ring: VecDeque<u32>,
}

impl RecentUids {
    /// 首次见到返回 true 并记录；重复返回 false。
    fn insert(&mut self, uid: u32) -> bool {
        if uid <= self.floor || self.ring.contains(&uid) {
            return false;
        }
        if self.ring.len() >= SESSION_DEDUP_CAPACITY {
            if let Some(evicted) = self.ring.pop_front() {
                self.floor = self.floor.max(evicted);
            }
        }
        self.ring.push_back(uid);
        self.highest = self.highest.max(uid);
        true
    }

    fn reset(&mut self) {
        *self = RecentUids::default();
    }
}

/// 一次正文抓取计划。
enum FetchPlan {
    /// 刚 SELECT 过且 UIDNEXT 表明游标之后没有新邮件：零正文开销。
    Nothing,
    /// `{start}:*` 一条命令流式处理（小增量的常态路径，与 v4 行为一致）。
    Stream,
    /// 已知 UID 清单，按 [`FETCH_BATCH_SIZE`] 分批抓取。`notify_from` 之前的
    /// 主文件夹邮件静默落库（冷启动截断，ADR-0041 决策 1）。
    Batches {
        uids: Vec<u32>,
        highest: u32,
        notify_from: u32,
    },
}

/// 一次连接会话的同步上下文：把原先散落在 `serve` 里的会话状态收拢到一处。
struct SyncSession<'a> {
    account: &'a str,
    ctx: &'a Context,
    capabilities: ServerCapabilities,
    initial_sync_limit: usize,
    /// 以服务器线上文件夹名为键；条目数受 `MAX_SYNCED_FOLDERS` 约束。
    folders: HashMap<String, FolderSession>,
    selected: Selected,
}

impl SyncSession<'_> {
    async fn refresh_on_wake(
        &mut self,
        session: &mut async_imap::Session<Conn>,
        cfg: &ReceiveConfig,
        receive: &Receive,
        targets: &mut Vec<SyncTarget>,
    ) -> Option<bool> {
        let request = receive.take_manual_sync();
        let outcome = match discover_folders(session, cfg, self.ctx).await {
            Err(error) => Err(error),
            Ok(mut refreshed) => {
                self.selected = None;
                let result = match &request {
                    Some((_, Some(name))) => {
                        match refreshed
                            .iter()
                            .enumerate()
                            .find(|(_, target)| &target.name == name)
                        {
                            Some((index, target)) => {
                                self.sync_folder(session, target, index == 0).await
                            }
                            None => Ok(()),
                        }
                    }
                    Some((_, None)) if !refreshed.is_empty() => {
                        match self.sync_folder(session, &refreshed[0], true).await {
                            Ok(()) => self
                                .sweep_secondary_folders(session, &refreshed)
                                .await
                                .and_then(|failure| failure.map_or(Ok(()), Err)),
                            Err(error) => Err(error),
                        }
                    }
                    _ => Ok(()),
                };
                if !refreshed.is_empty() {
                    refreshed.truncate(MAX_SYNCED_FOLDERS);
                    self.folders
                        .retain(|name, _| refreshed.iter().any(|target| &target.name == name));
                    *targets = refreshed;
                }
                result
            }
        };
        if let Err(error) = &outcome {
            eprintln!(
                "[maild/receive] 账户 {}: 手动同步失败: {error}",
                self.account
            );
        }
        request.map(|(id, folder)| {
            let swept_all = folder.is_none() && outcome.is_ok();
            receive.finish_manual_sync(id, outcome.map_err(|error| error.to_string()));
            swept_all
        })
    }

    fn folder_mut(&mut self, name: &str) -> &mut FolderSession {
        if !self.folders.contains_key(name) {
            self.folders.insert(name.to_string(), FolderSession::default());
        }
        self.folders.get_mut(name).expect("刚插入")
    }

    /// 逐个增量同步非主文件夹，结束后回到主文件夹（IDLE 必须挂在它上面）。
    ///
    /// 单个文件夹失败不终止整轮：一个权限受限或刚被删掉的 mailbox 不该让整账户掉线。
    async fn sweep_secondary_folders(
        &mut self,
        session: &mut async_imap::Session<Conn>,
        targets: &[SyncTarget],
    ) -> Result<Option<ReceiveError>, ReceiveError> {
        let mut first_error = None;
        for target in targets.iter().skip(1) {
            if let Err(error) = self.sync_folder(session, target, false).await {
                eprintln!(
                    "[maild/receive] 账户 {}: 同步文件夹 {} 失败（继续）: {error}",
                    self.account, target.display_name
                );
                self.selected = None;
                first_error.get_or_insert(error);
            }
        }
        if targets.len() > 1 {
            session
                .select(&targets[0].name)
                .await
                .map_err(sync_stage(format!("SELECT {} 失败", targets[0].display_name)))?;
            // 重新 SELECT 后 UIDVALIDITY 由下一次 sync_folder 重新读取。
            self.selected = None;
        }
        Ok(first_error)
    }

    /// 增量同步一个文件夹。
    ///
    /// 主文件夹走既有 `MailReceived` waterfall → `MailFiltered` 链路；其余文件夹直接落库，
    /// 不经过 filters、不触发 OTP、不发通知（ADR-0026 决策 9）。
    ///
    /// 低占用约束（ADR-0041 决策 1）：
    /// - 刚 SELECT 过时用 UIDNEXT 判断游标之后有没有新邮件，没有就一条 FETCH 都不发——
    ///   省掉 `N:*` 每次把最后一封旧邮件的整个正文重传一遍；
    /// - 冷启动（无游标）且信箱超过 `initial_sync_limit` 封时，只拉最新的那一段；
    /// - 待抓 UID 多时按 [`FETCH_BATCH_SIZE`] 分批。
    async fn sync_folder(
        &mut self,
        session: &mut async_imap::Session<Conn>,
        target: &SyncTarget,
        is_primary: bool,
    ) -> Result<(), ReceiveError> {
        let account = self.account;
        let ctx = self.ctx;
        ensure_selected(session, &mut self.selected, &target.name, self.capabilities.condstore)
            .await?;
        // UIDNEXT 只在刚 SELECT 完的那一刻可信：取走即作废，之后的同步回到 `N:*`。
        let (uid_validity, exists, fresh_uid_next) = match self.selected.as_mut() {
            Some(current) => (current.uid_validity, current.exists, current.uid_next.take()),
            None => (0, 0, None),
        };
        if uid_validity == 0 {
            // 没有 UIDVALIDITY 就没有 UID 空间的稳定性保证，增量游标失去意义。
            eprintln!(
                "[maild/receive] 账户 {account}: 文件夹 {} 未返回 UIDVALIDITY，跳过",
                target.display_name
            );
            return Ok(());
        }

        let mut start_uid: u32 = 1;
        match (store_of(ctx), target.folder_id) {
            (Some(store), Some(folder_id)) => {
                let record = store
                    .folder_by_name(account, &target.name)
                    .map_err(store_stage("读取文件夹游标失败"))?;
                if let Some(record) = record {
                    if record.uid_validity.is_some_and(|value| value != uid_validity) {
                        // UIDVALIDITY 翻转 = UID 空间已重编号，旧 UID 与新 UID 没有对应关系。
                        let removed = store
                            .rebuild_folder(folder_id, uid_validity)
                            .map_err(store_stage("重建文件夹失败"))?;
                        self.folder_mut(&target.name).recent.reset();
                        eprintln!(
                            "[maild/receive] 账户 {account}: 文件夹 {} UIDVALIDITY 变化，重建 {removed} 行",
                            target.display_name
                        );
                    } else if let Some(last) = record.last_seen_uid {
                        start_uid = last.saturating_add(1);
                    }
                }
            }
            _ => {
                if let Some(last) = self.folders.get(&target.name).and_then(|state| state.cursor) {
                    start_uid = last.saturating_add(1);
                }
            }
        }

        let plan = self
            .plan_fetch(session, start_uid, exists, fresh_uid_next)
            .await?;
        let mut highest = start_uid.saturating_sub(1);
        let mut batched = false;
        match plan {
            FetchPlan::Nothing => {}
            FetchPlan::Stream => {
                let query = format!("{start_uid}:*");
                let state = self.folder_mut(&target.name);
                fetch_and_deliver(
                    session, ctx, account, target, state, &query, start_uid, is_primary, 0,
                    &mut highest,
                )
                .await?;
            }
            FetchPlan::Batches {
                uids,
                highest: listed_highest,
                notify_from,
            } => {
                batched = !uids.is_empty();
                let state = self.folder_mut(&target.name);
                for chunk in uids.chunks(FETCH_BATCH_SIZE) {
                    let set = chunk.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
                    fetch_and_deliver(
                        session, ctx, account, target, state, &set, start_uid, is_primary,
                        notify_from, &mut highest,
                    )
                    .await?;
                }
                // 截断窗口之外的旧邮件不再是「新邮件」：游标直接推进到清单里最大的 UID。
                highest = highest.max(listed_highest);
            }
        }

        // 每个文件夹每次会话对账一次：对账是 O(mailbox)，不能挂在 IDLE 唤醒的热路径上。
        if let Some(folder_id) = target.folder_id {
            let first_time = !std::mem::replace(&mut self.folder_mut(&target.name).reconciled, true);
            if first_time {
                if let Err(error) = reconcile_folder_flags(
                    session,
                    target,
                    folder_id,
                    account,
                    ctx,
                    &self.selected,
                    self.capabilities,
                )
                .await
                {
                    eprintln!(
                        "[maild/receive] 账户 {account}: 文件夹 {} 标记对账失败（继续）: {error}",
                        target.display_name
                    );
                }
            }
        }

        let last_seen = (highest > 0).then_some(highest);
        match (store_of(ctx), target.folder_id) {
            (Some(store), Some(folder_id)) => {
                store
                    .advance_folder_cursor(folder_id, uid_validity, last_seen, now_unix())
                    .map_err(store_stage("更新文件夹游标失败"))?;
                if batched {
                    // 冷启动 / 大批补拉刚把一大段报文流过内存：收紧页缓存与 WAL，再把空闲堆页还给内核。
                    store.shrink_memory();
                }
            }
            _ => {
                if let Some(last) = last_seen {
                    self.folder_mut(&target.name).cursor = Some(last);
                }
            }
        }
        if batched {
            crate::daemon::release_unused_memory();
        }
        Ok(())
    }

    /// 决定这一轮怎么抓正文（ADR-0041 决策 1）。
    async fn plan_fetch(
        &mut self,
        session: &mut async_imap::Session<Conn>,
        start_uid: u32,
        exists: u32,
        fresh_uid_next: Option<u32>,
    ) -> Result<FetchPlan, ReceiveError> {
        if let Some(uid_next) = fresh_uid_next {
            // UIDNEXT ≤ start：游标之后不可能有邮件。
            if uid_next <= start_uid || exists == 0 {
                return Ok(FetchPlan::Nothing);
            }
        }
        let limit = self.initial_sync_limit;
        let cold = start_uid <= 1;
        if cold && limit > 0 && exists as usize > limit {
            // 冷启动大信箱：先只要 UID（几个字节一条），绝不发 `UID FETCH 1:* BODY.PEEK[]`。
            let mut uids = list_uids_for_cold_start(session, exists, limit).await?;
            let total = uids.len().max(exists as usize);
            let highest = uids.last().copied().unwrap_or(0);
            let truncated = total > limit;
            if uids.len() > limit {
                uids.drain(..uids.len() - limit);
            }
            let notify_from = if truncated {
                uids.len()
                    .checked_sub(COLD_START_NOTIFY_TAIL)
                    .map(|index| uids[index])
                    .unwrap_or(0)
            } else {
                0
            };
            if truncated {
                eprintln!(
                    "[maild/receive] 账户 {}: 冷启动信箱共 {total} 封，只同步最新 {} 封正文",
                    self.account,
                    uids.len()
                );
            }
            return Ok(FetchPlan::Batches {
                uids,
                highest,
                notify_from,
            });
        }
        // 已知积压超过一批（长时间离线后重连）：先取 UID 清单再分批。
        let backlog = fresh_uid_next.map(|next| next.saturating_sub(start_uid) as usize);
        if backlog.is_some_and(|count| count > FETCH_BATCH_SIZE) {
            let uids = list_uids_from(session, start_uid).await?;
            let highest = uids.last().copied().unwrap_or(0);
            return Ok(FetchPlan::Batches {
                uids,
                highest,
                notify_from: 0,
            });
        }
        Ok(FetchPlan::Stream)
    }

    /// 在当前连接上按需拉取一封并回传结果（ADR-0041 决策 4）。不用 `?`：一次失败不该
    /// 把收信连接带走，真正的连接故障会在下一次同步时暴露。
    async fn serve_fetch_by_uid(
        &mut self,
        session: &mut async_imap::Session<Conn>,
        targets: &[SyncTarget],
        request: FetchByUidRequest,
    ) {
        let outcome = self
            .fetch_by_uid(session, targets, &request.folder, request.uid)
            .await;
        let _ = request.reply.send(outcome);
    }

    async fn fetch_by_uid(
        &mut self,
        session: &mut async_imap::Session<Conn>,
        targets: &[SyncTarget],
        folder: &str,
        uid: u32,
    ) -> Result<Option<MailMessage>, String> {
        // 按需拉取是 RPC，报错仍是协议 `error.message`（ADR-0048 L6 不涉及）。
        ensure_selected(session, &mut self.selected, folder, self.capabilities.condstore)
            .await
            .map_err(|error| error.to_string())?;
        let role = match targets.iter().find(|target| target.name == folder) {
            Some(target) => target.role,
            None => store_of(self.ctx)
                .and_then(|store| store.folder_by_name(self.account, folder).ok().flatten())
                .and_then(|record| record.role),
        };
        let now = now_unix();
        let mut found: Option<MailMessage> = None;
        {
            let fetches = session
                .uid_fetch(uid.to_string(), "(UID FLAGS INTERNALDATE BODY.PEEK[])")
                .await
                .map_err(|error| format!("按需拉取 FETCH 失败: {error}"))?;
            futures::pin_mut!(fetches);
            while let Some(fetch) = fetches.next().await {
                let fetch = fetch.map_err(|error| format!("按需拉取 FETCH 流出错: {error}"))?;
                // 服务器可能夹带别的 FETCH（例如并发的标记变化）；只认请求的那一封。
                if fetch.uid != Some(uid) || found.is_some() {
                    continue;
                }
                let Some(body) = fetch.body() else { continue };
                let received_at = arrival_from_fetch(&fetch, now);
                let mut message = parse_message_at(self.account, &uid.to_string(), body, received_at);
                message.folder = folder.to_string();
                message.folder_role = FolderRole::to_column(role).to_string();
                apply_flags(&mut message, &fetch);
                found = Some(message);
            }
        }
        if let (Some(message), Some(store)) = (&found, store_of(self.ctx)) {
            store
                .insert_message(message)
                .map_err(|error| format!("按需拉取落库失败: {error}"))?;
        }
        Ok(found)
    }
}

/// 冷启动的 UID 清单：用 `UID SEARCH <seq>:*` 只要最新 `limit` 封的 UID；服务器不支持或
/// 结果异常时回退到 `UID FETCH 1:* (UID)`（每条只有几个字节）。返回升序。
async fn list_uids_for_cold_start(
    session: &mut async_imap::Session<Conn>,
    exists: u32,
    limit: usize,
) -> Result<Vec<u32>, ReceiveError> {
    let from_seq = (exists as usize).saturating_sub(limit).saturating_add(1);
    if let Ok(hits) = session.uid_search(format!("{from_seq}:*")).await {
        let mut uids: Vec<u32> = hits.into_iter().collect();
        if !uids.is_empty() {
            uids.sort_unstable();
            uids.dedup();
            return Ok(uids);
        }
    }
    let mut uids = fetch_uid_list(session, "1:*").await?;
    uids.sort_unstable();
    uids.dedup();
    Ok(uids)
}

/// 游标之后的 UID 清单（增量积压分批用）。返回升序，已滤掉 `< start_uid` 的那条
/// （RFC 3501：`N:*` 在 N 大于最大 UID 时仍返回最大 UID）。
async fn list_uids_from(
    session: &mut async_imap::Session<Conn>,
    start_uid: u32,
) -> Result<Vec<u32>, ReceiveError> {
    let range = format!("{start_uid}:*");
    let mut uids: Vec<u32> = match session.uid_search(format!("UID {range}")).await {
        Ok(hits) if !hits.is_empty() => hits.into_iter().collect(),
        _ => fetch_uid_list(session, &range).await?,
    };
    uids.retain(|uid| *uid >= start_uid);
    uids.sort_unstable();
    uids.dedup();
    Ok(uids)
}

/// `UID FETCH <range> (UID)`：只取 UID，不带标记与正文。
async fn fetch_uid_list(
    session: &mut async_imap::Session<Conn>,
    range: &str,
) -> Result<Vec<u32>, ReceiveError> {
    let fetches = session
        .uid_fetch(range, "(UID)")
        .await
        .map_err(sync_stage("UID 清单 FETCH 失败"))?;
    futures::pin_mut!(fetches);
    let mut uids = Vec::new();
    while let Some(fetch) = fetches.next().await {
        let fetch = fetch.map_err(sync_stage("UID 清单 FETCH 流出错"))?;
        if let Some(uid) = fetch.uid {
            uids.push(uid);
        }
    }
    Ok(uids)
}

/// 抓一组 UID 的完整报文并逐封投递。报文以流的形式逐封解析，同一时刻只有一封驻留内存。
///
/// `notify_from > 0` 时，主文件夹里 UID 小于它的邮件静默落库而不走实时通知链路
/// （冷启动截断时防止一次性轰炸几百条通知，ADR-0041 决策 1）。
#[allow(clippy::too_many_arguments)]
async fn fetch_and_deliver(
    session: &mut async_imap::Session<Conn>,
    ctx: &Context,
    account: &str,
    target: &SyncTarget,
    state: &mut FolderSession,
    uid_set: &str,
    start_uid: u32,
    is_primary: bool,
    notify_from: u32,
    highest: &mut u32,
) -> Result<(), ReceiveError> {
    let fetches = session
        .uid_fetch(uid_set, "(UID FLAGS INTERNALDATE BODY.PEEK[])")
        .await
        .map_err(sync_stage("UID FETCH 失败"))?;
    futures::pin_mut!(fetches);
    let now = now_unix();
    while let Some(fetch) = fetches.next().await {
        let fetch = fetch.map_err(sync_stage("FETCH 流出错"))?;
        let Some(uid) = fetch.uid else { continue };
        // RFC 3501 的 `N:*`：当 N 大于最大 UID 时服务器仍会返回最大 UID 那一条。
        // 不过滤就会每次重复处理最后一封。
        if uid < start_uid {
            continue;
        }
        *highest = (*highest).max(uid);
        if !state.recent.insert(uid) {
            continue;
        }
        let Some(body) = fetch.body() else { continue };
        let received_at = arrival_from_fetch(&fetch, now);
        let mut message = parse_message_at(account, &uid.to_string(), body, received_at);
        message.folder = target.name.clone();
        message.folder_role = FolderRole::to_column(target.role).to_string();
        apply_flags(&mut message, &fetch);

        if is_primary && uid >= notify_from {
            // `mail/received` waterfall 过滤链；R4 无 filters 插件，终端默认 Accept
            let decision = ctx
                .waterfall::<MailReceived, _>(message.clone(), |_m| {
                    Box::pin(async move { FilterDecision::Accept })
                })
                .await;
            if decision == FilterDecision::Accept {
                ctx.emit::<MailFiltered>(message);
            }
        } else if let Some(store) = store_of(ctx) {
            if let Err(error) = store.insert_message(&message) {
                eprintln!("[maild/receive] 账户 {account}: 静默落库失败: {error}");
            }
        }
    }
    Ok(())
}

fn arrival_from_fetch(fetch: &async_imap::types::Fetch, now: u64) -> u64 {
    fetch
        .internal_date()
        .and_then(|dt| u64::try_from(dt.timestamp()).ok())
        .map(|ts| mime::sanitize_arrival_secs(ts, now))
        .unwrap_or(now)
}

/// MIME → [`MailMessage`]：解析 RFC 5322 事实、正文（纯文本 + HTML 原文）与附件（ADR-0025）。
pub fn parse_message(account: &str, uid: &str, raw: &[u8]) -> MailMessage {
    let received_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    parse_message_at(account, uid, raw, received_at)
}

/// [`parse_message`] 的可注入到达时间版本。`received_at` 是最佳已知到达时刻（IMAP
/// `INTERNALDATE`、provider 收信时刻，缺失时回退本机首次观测时刻），与 `Date` 头无关；
/// 显式传入使排序与可信度判定可确定性测试。
pub fn parse_message_at(account: &str, uid: &str, raw: &[u8], received_at: u64) -> MailMessage {
    let parsed = parse_mail(raw).ok();
    let get = |name: &str| -> String {
        parsed
            .as_ref()
            .and_then(|m| m.get_headers().get_first_value(name))
            .unwrap_or_default()
    };

    let parts = parsed.as_ref().map(mime::collect_parts).unwrap_or_default();
    // text/plain 优先；只有 HTML 的邮件走真正的 HTML → 文本转换，
    // 而不是删标签（`<style>` / `<script>` 的内容不得进正文）。
    let mut body = if parts.text.is_empty() {
        mime::html_to_text(&parts.html)
    } else {
        parts.text
    };
    mime::truncate_utf8_bytes(&mut body, mime::MAX_STORED_TEXT_BYTES);
    let mut body_html = parts.html;
    if mime::truncate_utf8_bytes(&mut body_html, mime::MAX_STORED_HTML_BYTES) {
        body_html.push_str(mime::HTML_TRUNCATION_MARKER);
    }
    let snippet = mime::snippet_of(&body);

    let date_at = mime::parse_date_header(&get("Date"));
    let sort_at = mime::sort_key(date_at, received_at);
    let attachments = parts.attachments;
    let has_attachments = attachments.iter().any(|attachment| !attachment.inline);

    MailMessage {
        account: account.to_string(),
        uid: uid.to_string(),
        sender: get("From"),
        // ADR-0022：收件人取 To 头首值，有界脱敏（展示性元数据）。
        recipient: sanitize_detail_owned(&get("To")),
        subject: get("Subject"),
        body,
        received_at,
        message_id: mime::sanitize_header(
            &mime::strip_angle_brackets(&get("Message-ID")),
            mime::MAX_MESSAGE_ID_BYTES,
        ),
        in_reply_to: mime::sanitize_header(
            &mime::strip_angle_brackets(&get("In-Reply-To")),
            mime::MAX_MESSAGE_ID_BYTES,
        ),
        references: mime::sanitize_header(&get("References"), mime::MAX_REFERENCES_BYTES),
        cc: mime::sanitize_header(&get("Cc"), mime::MAX_ADDRESS_LIST_BYTES),
        bcc: mime::sanitize_header(&get("Bcc"), mime::MAX_ADDRESS_LIST_BYTES),
        reply_to: mime::sanitize_header(&get("Reply-To"), mime::MAX_ADDRESS_LIST_BYTES),
        date_at,
        sort_at,
        body_html,
        snippet,
        has_attachments,
        // 标记位由 M4.5-05 的写路径与同步填充；解析阶段一律 false。
        seen: false,
        flagged: false,
        answered: false,
        deleted: false,
        attachments,
        // 文件夹由同步路径填充：解析器只看报文，报文里没有「它在哪个信箱」。
        folder: String::new(),
        folder_role: String::new(),
        // 线程由 store 在落库时归并：它要查库里已有的边，解析阶段判定不了。
        thread_id: 0,
    }
}

/// 为受管账户启动收信循环（ADR-0015 §7）。
///
/// 与 legacy 路径的区别：不经插件装配，事件走独立 realm scope（账户事件只见本账户 + ROOT），
/// 秘密经 [`SecretFetcher`] 每次连接前取入短生命周期缓冲。停止协议由调用方
/// （AccountManager）负责：`receive.shutdown()` 后 await 任务句柄，LIFO 逆序释放。
pub fn spawn_managed_receive(
    ctx: &Context,
    realm: RealmId,
    cfg: ReceiveConfig,
    secret_fetcher: SecretFetcher,
    scheduler: Option<Scheduler>,
) -> Result<(Receive, tokio::task::JoinHandle<()>), String> {
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| "受管账户收信循环需要在 tokio 运行时内启动".to_string())?;
    let scoped = ctx.scoped(realm);
    let receive = Receive::new(&cfg.account);
    let task = handle.spawn(run_loop(
        cfg,
        scoped,
        receive.clone(),
        scheduler,
        Some(secret_fetcher),
    ));
    Ok((receive, task))
}

// —— 委托收信（M4-05，ADR-0021）——

/// 委托收信批大小上限：响应必须落在 1 MiB 单帧内，内核侧 clamp，不信插件。
pub(crate) const PROVIDER_RECEIVE_BATCH_MAX: u64 = 20;
/// 单次轮询 tick 的连续拉取预算：超过后留待下一 tick 继续（防慢 provider 拖住任务）。
const PROVIDER_DRAIN_BUDGET: Duration = Duration::from_secs(90);

/// 委托收信的运行参数。
#[derive(Clone, Debug)]
pub(crate) struct ProviderReceiveSpec {
    pub(crate) account: String,
    /// 账户的规范远端邮箱地址；与可变 alias recipient 分开传递。
    pub(crate) address: String,
    pub(crate) provider_id: String,
    pub(crate) poll_secs: u64,
}

/// 为委托 incoming 的账户启动轮询收信循环。v1 仅 poll（无 IDLE 等价物），
/// 游标是库内最大数字 uid；停止协议与 [`spawn_managed_receive`] 相同。
pub(crate) fn spawn_provider_receive(
    ctx: &Context,
    realm: RealmId,
    spec: ProviderReceiveSpec,
) -> Result<(Receive, tokio::task::JoinHandle<()>), String> {
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| "委托账户收信循环需要在 tokio 运行时内启动".to_string())?;
    let scoped = ctx.scoped(realm);
    let receive = Receive::new(&spec.account);
    let task = handle.spawn(run_provider_loop(spec, scoped, receive.clone()));
    Ok((receive, task))
}

async fn run_provider_loop(spec: ProviderReceiveSpec, ctx: Context, receive: Receive) {
    let mut first = true;
    let mut sync_rx = receive.sync_rx();
    let mut shutdown_rx = receive.shutdown_rx();
    loop {
        if receive.is_shutdown() {
            return;
        }
        receive.set_state(
            &ctx,
            if first {
                AccountStateKind::Connecting
            } else {
                AccountStateKind::Reconnecting
            },
        );
        first = false;
        let request = receive.take_manual_sync();
        let result = provider_drain(&spec, &ctx, &receive).await;
        if let Some((id, _)) = request {
            receive
                .finish_manual_sync(id, result.as_ref().map(|_| ()).map_err(ToString::to_string));
        }
        match result {
            Ok(count) => {
                eprintln!(
                    "[maild/provider] 委托收信 account={} provider={} outcome=delivered count={count}",
                    spec.account, spec.provider_id
                );
            }
            Err(error) => {
                receive.set_offline(&ctx, &error);
                receive.set_online(false);
                eprintln!(
                    "[maild/provider] 委托收信 account={} provider={} outcome=failed detail={error}",
                    spec.account, spec.provider_id
                );
                if receive.is_shutdown() {
                    return;
                }
                // 失败退避复用 reconnect 节奏的下限，避免紧贴 provider 重试。
                if !receive.backoff_sleep(Duration::from_secs(5)).await {
                    return;
                }
                sync_rx.borrow_and_update();
                continue;
            }
        }
        if receive.is_shutdown() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(spec.poll_secs.max(1))) => {}
            _ = sync_rx.changed() => {}
            _ = shutdown_rx.changed() => {}
        }
    }
}

/// 一个轮询 tick：注册检查 → 以库内 max(uid) 为游标分批拉取 → 投递 waterfall。
/// 成功返回本 tick 实际投递条数。
async fn provider_drain(
    spec: &ProviderReceiveSpec,
    ctx: &Context,
    receive: &Receive,
) -> Result<usize, ReceiveError> {
    let registry = ctx
        .get::<crate::provider::ProviderRegistry>()
        .ok_or_else(|| ReceiveError::new(AccountDetailCode::Internal, "provider 注册表未装配"))?;
    let endpoint = registry.endpoint_of(&spec.provider_id).ok_or_else(|| {
        ReceiveError::new(
            AccountDetailCode::ProviderUnavailable,
            format!("provider {} 未注册", spec.provider_id),
        )
    })?;
    let store = ctx
        .get::<Store>()
        .ok_or_else(|| ReceiveError::new(AccountDetailCode::Internal, "store 服务未装配"))?;
    let mut since = store
        .max_numeric_uid(&spec.account)
        .map_err(store_stage("游标查询失败"))?
        .unwrap_or(0);

    receive.set_state(ctx, AccountStateKind::Online);
    receive.set_online(true);

    let deadline = std::time::Instant::now() + PROVIDER_DRAIN_BUDGET;
    let mut delivered = 0_usize;
    loop {
        let value = endpoint
            .call(
                "transport/receive",
                serde_json::to_value(TransportReceiveRequest {
                    account: spec.account.clone(),
                    address: spec.address.clone(),
                    since_uid: since.to_string(),
                    max: PROVIDER_RECEIVE_BATCH_MAX,
                })
                .expect("transport receive DTO is serializable"),
                provider_call_budget(),
            )
            .await
            .map_err(provider_call_detail)?;
        let messages = parse_provider_messages(&spec.account, &value)?;
        if messages.is_empty() {
            return Ok(delivered);
        }
        let batch_len = messages.len() as u64;
        for msg in messages {
            if let Ok(uid) = msg.uid.parse::<u64>() {
                since = since.max(uid);
            }
            // 与 IMAP 路径同一条过滤链：store/filters/OTP/notify/推送全复用。
            let decision = ctx
                .waterfall::<MailReceived, _>(msg.clone(), |_m| {
                    Box::pin(async move { FilterDecision::Accept })
                })
                .await;
            if decision == FilterDecision::Accept {
                ctx.emit::<MailFiltered>(msg);
                delivered += 1;
            }
        }
        if batch_len < PROVIDER_RECEIVE_BATCH_MAX || std::time::Instant::now() >= deadline {
            return Ok(delivered);
        }
    }
}

/// 反向调用失败 → 循环可展示的 detail（不含插件消息体内容）。
///
/// `Unavailable` 的说明是 provider 模块自己写的（连接已关闭、端点已失效），不是插件原文，
/// 不作为 `server_detail` 透出；`Upstream` 的消息来自插件，脱敏后透出。
fn provider_call_detail(error: crate::provider::ProviderCallError) -> ReceiveError {
    match error {
        crate::provider::ProviderCallError::Unavailable(detail) => ReceiveError::new(
            AccountDetailCode::ProviderUnavailable,
            format!("provider 不可用: {detail}"),
        ),
        crate::provider::ProviderCallError::Timeout => {
            ReceiveError::new(AccountDetailCode::ProviderTimeout, "provider 响应超时")
        }
        crate::provider::ProviderCallError::Upstream { code, message } => ReceiveError::with_detail(
            AccountDetailCode::ProviderFailed,
            format!("provider 上报失败({code})"),
            crate::probe::sanitize_detail(&message),
        ),
    }
}

/// 校验并转换 transport/receive 应答。uid 必须是不超过 20 位十进制数字；
/// 字段长度沿用协议上限；account 由内核覆盖（不信任载荷中的同名键）。
fn parse_provider_messages(
    account: &str,
    value: &serde_json::Value,
) -> Result<Vec<MailMessage>, ReceiveError> {
    let invalid =
        |message: &str| ReceiveError::new(AccountDetailCode::ProviderResponseInvalid, message);
    let response: TransportReceiveResponse = serde_json::from_value(value.clone())
        .map_err(|_| invalid("provider receive 应答结构无效"))?;
    let now = now_unix();
    let mut out = Vec::with_capacity(response.messages.len());
    for item in response.messages {
        let uid = item.uid;
        if uid.is_empty() || uid.len() > 20 || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid("provider 返回非法 uid（必须是十进制数字）"));
        }
        if item.received_at > mime::LATEST_PLAUSIBLE_DATE_SECS {
            return Err(invalid("provider 返回非法 received_at"));
        }
        let received_at = if item.received_at >= mime::EARLIEST_PLAUSIBLE_DATE_SECS {
            mime::sanitize_arrival_secs(item.received_at, now)
        } else {
            item.received_at
        };
        let sender = sanitize_detail_owned(&item.sender);
        // recipient 是 ADR-0022 增补的可选键（地址层溯源）；缺省为空。
        let recipient = sanitize_detail_owned(&item.recipient);
        let subject = sanitize_detail_owned(&item.subject);
        let mut body = item.body;
        if body.len() > maild_client::MAX_BODY_BYTES {
            return Err(invalid("provider 消息正文超过协议上限"));
        }
        mime::truncate_utf8_bytes(&mut body, mime::MAX_STORED_TEXT_BYTES);
        // ADR-0025：provider 合同新增的可选头部键，缺省保持既有行为。
        let date_at = mime::parse_date_header(&item.date);
        let sort_at = mime::sort_key(date_at, received_at);
        let snippet = mime::snippet_of(&body);
        out.push(MailMessage {
            account: account.to_string(),
            uid,
            sender,
            recipient,
            subject,
            body,
            received_at,
            message_id: mime::sanitize_header(
                &mime::strip_angle_brackets(&item.message_id),
                mime::MAX_MESSAGE_ID_BYTES,
            ),
            cc: mime::sanitize_header(&item.cc, mime::MAX_ADDRESS_LIST_BYTES),
            reply_to: mime::sanitize_header(&item.reply_to, mime::MAX_ADDRESS_LIST_BYTES),
            date_at,
            sort_at,
            snippet,
            ..MailMessage::default()
        });
    }
    Ok(out)
}

fn sanitize_detail_owned(value: &str) -> String {
    let mut sanitized: String = value.chars().filter(|ch| !ch.is_control()).collect();
    if sanitized.len() > 512 {
        sanitized = sanitized.chars().take(512).collect();
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_uids_stay_bounded_and_reject_replays() {
        let mut recent = RecentUids::default();
        for uid in 1..=10_000 {
            assert!(recent.insert(uid), "首次见到 {uid}");
        }
        assert_eq!(recent.ring.len(), SESSION_DEDUP_CAPACITY, "环容量有界");
        assert_eq!(recent.highest, 10_000);
        // 环内的重放与早已挤出环的旧 UID 都视为已处理。
        assert!(!recent.insert(10_000));
        assert!(!recent.insert(9_999));
        assert!(!recent.insert(1));
        assert!(recent.insert(10_001));
        recent.reset();
        assert!(recent.insert(1), "UIDVALIDITY 翻转后重新计数");
    }

    #[test]
    fn reconnect_backoff_grows_exponentially_and_is_capped() {
        let within = |delay: Duration, base: u64| {
            delay >= Duration::from_secs(base) && delay <= Duration::from_millis(base * 1100)
        };
        assert!(within(reconnect_backoff(5, 0), 5), "首次失败按基数");
        assert!(within(reconnect_backoff(5, 1), 10));
        assert!(within(reconnect_backoff(5, 3), 40));
        assert!(within(reconnect_backoff(5, 6), 300), "封顶 300s");
        assert!(within(reconnect_backoff(5, 60), 300), "指数封顶不溢出");
        // 基数本身超过上限时尊重配置。
        assert!(within(reconnect_backoff(3_600, 2), 3_600));
        assert!(within(reconnect_backoff(0, 0), 1), "基数下界 1s");
    }

    #[test]
    fn initial_sync_limit_defaults_and_merges() {
        let base = ReceiveConfig::default();
        assert_eq!(base.initial_sync_limit, DEFAULT_INITIAL_SYNC_LIMIT);
        let parsed: ReceiveConfig = toml::from_str("account = \"a\"").unwrap();
        assert_eq!(parsed.initial_sync_limit, DEFAULT_INITIAL_SYNC_LIMIT);
        let frag = ReceiveConfig {
            initial_sync_limit: 0,
            ..ReceiveConfig::default()
        };
        assert_eq!(base.merge(&frag).initial_sync_limit, DEFAULT_INITIAL_SYNC_LIMIT, "零值不覆盖");
        let frag = ReceiveConfig {
            initial_sync_limit: 50,
            ..ReceiveConfig::default()
        };
        assert_eq!(base.merge(&frag).initial_sync_limit, 50);
    }

    #[test]
    fn provider_messages_reject_millisecond_received_at_and_clamp_far_future() {
        let bad = serde_json::json!({
            "messages": [{
                "uid": "1",
                "sender": "s@example.test",
                "subject": "ms bug",
                "body": "hi",
                "received_at": 1_795_000_000_000_u64
            }]
        });
        let err = parse_provider_messages("acc", &bad).unwrap_err().to_string();
        assert!(err.contains("received_at"), "{err}");

        let now = now_unix();
        let future = serde_json::json!({
            "messages": [{
                "uid": "2",
                "sender": "s@example.test",
                "subject": "future",
                "body": "hi",
                "received_at": 4_102_444_800_u64
            }]
        });
        let parsed = parse_provider_messages("acc", &future).unwrap();
        assert!(parsed[0].received_at <= now.saturating_add(60));
    }

    // —— ADR-0048 L6：每个 detail code 至少一条真实触发路径，整句与改造前逐字节一致 ——

    use tokio::io::AsyncBufReadExt;

    /// 本线程临时缩短分阶段超时；guard 释放时恢复。`#[tokio::test]` 是 current_thread，
    /// 被测 future 与它 spawn 的任务都跑在测试线程上。
    struct ShortTimeouts;

    impl ShortTimeouts {
        fn apply(patch: impl FnOnce(&mut StageTimeouts)) -> ShortTimeouts {
            let mut timeouts = STAGE_TIMEOUTS;
            patch(&mut timeouts);
            TEST_STAGE_TIMEOUTS.with(|cell| cell.set(Some(timeouts)));
            ShortTimeouts
        }
    }

    impl Drop for ShortTimeouts {
        fn drop(&mut self) {
            TEST_STAGE_TIMEOUTS.with(|cell| cell.set(None));
        }
    }

    const SHORT: Duration = Duration::from_millis(200);

    /// 单连接脚本化 IMAP 服务器：先写 `greeting`，之后每读到一行命令就写回
    /// `reply(tag, 命令名)`；`None` 表示不应答（模拟半开连接）。连接保持到客户端挂断。
    async fn scripted_server(greeting: &'static str, reply: fn(&str, &str) -> Option<String>) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else { return };
            let (read, mut write) = stream.into_split();
            if write.write_all(greeting.as_bytes()).await.is_err() {
                return;
            }
            let mut lines = tokio::io::BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut parts = line.splitn(3, ' ');
                let tag = parts.next().unwrap_or_default().to_string();
                let command = parts.next().unwrap_or_default().to_ascii_uppercase();
                if let Some(text) = reply(&tag, &command) {
                    if write.write_all(text.as_bytes()).await.is_err() {
                        return;
                    }
                }
            }
        });
        port
    }

    /// 接受连接后一言不发、只读不写（TLS 握手卡住）。
    async fn silent_server() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut sink = [0u8; 512];
            while matches!(stream.read(&mut sink).await, Ok(n) if n > 0) {}
        });
        port
    }

    /// 能走到 IDLE 的应答：CAPABILITY、SELECT 正常，其余交给 `rest`。
    fn session_reply(tag: &str, command: &str) -> Option<String> {
        match command {
            "LOGIN" => Some(format!("{tag} OK LOGIN completed\r\n")),
            "CAPABILITY" => Some(format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK done\r\n")),
            "SELECT" => Some(format!(
                "* 0 EXISTS\r\n* OK [UIDVALIDITY 7] ok\r\n* OK [UIDNEXT 1] ok\r\n{tag} OK [READ-WRITE] done\r\n"
            )),
            "LOGOUT" => Some(format!("* BYE\r\n{tag} OK bye\r\n")),
            _ => None,
        }
    }

    fn config(port: u16, tls: &str) -> ReceiveConfig {
        ReceiveConfig {
            account: "acc".into(),
            server: "127.0.0.1".into(),
            port,
            username: "dev".into(),
            password: "dev".into(),
            tls: tls.into(),
            reconnect_secs: 3600,
            ..ReceiveConfig::default()
        }
    }

    async fn serve_once(cfg: &ReceiveConfig, secret: &[u8]) -> ReceiveError {
        let ctx = Context::new();
        let receive = Receive::new(&cfg.account);
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            serve(cfg, &ctx, &receive, None, SecretBuffer::new(secret.to_vec())),
        )
        .await
        .expect("serve 必须在时限内失败");
        outcome.expect_err("serve 应失败")
    }

    fn assert_fixed(error: &ReceiveError, code: AccountDetailCode, sentence: &str) {
        assert_eq!(error.code(), code, "{error}");
        assert_eq!(error.to_string(), sentence);
        assert_eq!(error.server_detail(), "", "我们自己写的整句不透出原文: {error}");
    }

    /// 整句 = `前缀: 原文`，`server_detail` 就是原文。
    fn assert_prefixed(error: &ReceiveError, code: AccountDetailCode, prefix: &str) {
        assert_eq!(error.code(), code, "{error}");
        assert!(!error.server_detail().is_empty(), "{error}");
        assert_eq!(error.to_string(), format!("{prefix}: {}", error.server_detail()));
    }

    #[test]
    fn detail_codes_serialize_as_their_stable_names() {
        let mut seen = std::collections::BTreeSet::new();
        for code in AccountDetailCode::ALL {
            assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
            assert!(seen.insert(code.as_str()), "重复的 code {}", code.as_str());
        }
        let info = AccountStateInfo {
            detail: "IMAP 登录失败: no response: bad".into(),
            detail_code: Some(AccountDetailCode::LoginFailed),
            server_detail: "no response: bad".into(),
            ..AccountStateInfo::plain("acc", AccountStateKind::Offline)
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            serde_json::json!({
                "account": "acc",
                "state": "offline",
                "detail": "IMAP 登录失败: no response: bad",
                "detail_code": "login_failed",
                "server_detail": "no response: bad",
            })
        );
        // 在线状态不带这两个键，老客户端看到的帧与以前相同。
        assert_eq!(
            serde_json::to_value(AccountStateInfo::plain("acc", AccountStateKind::Online)).unwrap(),
            serde_json::json!({"account": "acc", "state": "online", "detail": ""})
        );
    }

    #[test]
    fn server_detail_is_sanitized_but_the_sentence_is_not() {
        let raw = format!("line\r\nbreak {}", "x".repeat(400));
        let error = ReceiveError::with_detail(AccountDetailCode::SyncFailed, "LIST 失败", &raw);
        assert_eq!(error.to_string(), format!("LIST 失败: {raw}"), "整句逐字节保留");
        assert!(error.server_detail().len() <= 160);
        assert!(!error.server_detail().contains('\n'));
    }

    #[tokio::test]
    async fn connect_failed_reaches_the_account_state_with_code_and_os_text() {
        // 先占一个端口再关掉：连接被拒绝。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let cfg = config(port, "none");
        let ctx = Context::new();
        let receive = Receive::new(&cfg.account);
        let task = tokio::spawn(run_loop(cfg, ctx, receive.clone(), None, None));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while receive.state().state != AccountStateKind::Offline {
            assert!(tokio::time::Instant::now() < deadline, "应进入离线");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let state = receive.state();
        receive.shutdown();
        task.await.unwrap();
        assert_eq!(state.detail_code, Some(AccountDetailCode::ConnectFailed));
        assert!(!state.server_detail.is_empty());
        assert_eq!(
            state.detail,
            format!("连接 127.0.0.1:{port} 失败: {}", state.server_detail),
            "detail 整句与改造前相同"
        );
    }

    #[tokio::test]
    async fn connect_timeout_when_the_connect_budget_is_spent() {
        let _short = ShortTimeouts::apply(|t| t.connect_total = Duration::ZERO);
        let error = connect_client(&config(1, "none")).await.err().unwrap();
        assert_fixed(&error, AccountDetailCode::ConnectTimeout, "连接 127.0.0.1:1 失败: 连接超时");
    }

    #[tokio::test]
    async fn tls_failed_when_the_server_does_not_speak_tls() {
        let port = scripted_server("* OK plain text, not TLS\r\n", |_, _| None).await;
        let error = connect_client(&config(port, "implicit")).await.err().unwrap();
        assert_prefixed(&error, AccountDetailCode::TlsFailed, "TLS 握手失败");
    }

    #[tokio::test]
    async fn tls_timeout_when_the_handshake_hangs() {
        let _short = ShortTimeouts::apply(|t| t.handshake = SHORT);
        let port = silent_server().await;
        let error = connect_client(&config(port, "implicit")).await.err().unwrap();
        assert_fixed(&error, AccountDetailCode::TlsTimeout, "TLS / STARTTLS 握手超时");
    }

    #[tokio::test]
    async fn greeting_invalid_and_starttls_unsupported_carry_the_server_line() {
        let port = scripted_server("* BYE go away\r\n", |_, _| None).await;
        let error = connect_client(&config(port, "starttls")).await.err().unwrap();
        assert_prefixed(&error, AccountDetailCode::GreetingInvalid, "IMAP greeting 异常");
        assert_eq!(error.server_detail(), "* BYE go away");

        let port = scripted_server("* OK ready\r\n", |tag, _| Some(format!("{tag} BAD no tls\r\n"))).await;
        let error = connect_client(&config(port, "starttls")).await.err().unwrap();
        assert_prefixed(&error, AccountDetailCode::StarttlsUnsupported, "服务器不支持 STARTTLS");
        assert_eq!(error.server_detail(), "a0 BAD no tls");
    }

    /// 对端在 greeting 行尾前关闭：`read_line_exact` 自己写的中文不作为原文透出。
    #[tokio::test]
    async fn greeting_cut_off_hides_our_own_eof_text() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let _ = stream.write_all(b"* OK").await;
            }
        });
        let error = connect_client(&config(port, "starttls")).await.err().unwrap();
        assert_fixed(
            &error,
            AccountDetailCode::GreetingInvalid,
            "读 greeting 失败: 连接在行尾前关闭",
        );
    }

    #[tokio::test]
    async fn login_failed_carries_the_server_response() {
        let port = scripted_server("* OK ready\r\n", |tag, command| match command {
            "LOGIN" => Some(format!("{tag} NO [AUTHENTICATIONFAILED] bad credentials\r\n")),
            _ => None,
        })
        .await;
        let error = serve_once(&config(port, "none"), b"wrong").await;
        assert_prefixed(&error, AccountDetailCode::LoginFailed, "IMAP 登录失败");
        assert!(error.server_detail().contains("bad credentials"), "{error}");
    }

    #[tokio::test]
    async fn login_timeout_when_login_is_never_answered() {
        let _short = ShortTimeouts::apply(|t| t.login = SHORT);
        let port = scripted_server("* OK ready\r\n", |_, _| None).await;
        let error = serve_once(&config(port, "none"), b"dev").await;
        assert_fixed(&error, AccountDetailCode::LoginTimeout, "IMAP 登录超时");
    }

    #[tokio::test]
    async fn credential_invalid_when_the_secret_is_not_utf8() {
        let port = scripted_server("* OK ready\r\n", |_, _| None).await;
        let error = serve_once(&config(port, "none"), &[0xff, 0xfe]).await;
        assert_fixed(&error, AccountDetailCode::CredentialInvalid, "账户密码不是有效的 UTF-8 文本");
    }

    #[tokio::test]
    async fn credential_unavailable_reaches_the_account_state_without_our_text() {
        let cfg = config(1, "none");
        let ctx = Context::new();
        let receive = Receive::new(&cfg.account);
        let fetcher: SecretFetcher = Arc::new(|| {
            Err(ReceiveError::new(
                AccountDetailCode::CredentialUnavailable,
                "凭据读取失败: 钥匙串已锁定",
            ))
        });
        let task = tokio::spawn(run_loop(cfg, ctx, receive.clone(), None, Some(fetcher)));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while receive.state().state != AccountStateKind::Offline {
            assert!(tokio::time::Instant::now() < deadline, "应进入离线");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let state = receive.state();
        receive.shutdown();
        task.await.unwrap();
        assert_eq!(state.detail, "凭据读取失败: 钥匙串已锁定");
        assert_eq!(state.detail_code, Some(AccountDetailCode::CredentialUnavailable));
        assert_eq!(state.server_detail, "");
    }

    #[tokio::test]
    async fn sync_failed_when_the_primary_folder_cannot_be_selected() {
        let port = scripted_server("* OK ready\r\n", |tag, command| match command {
            "SELECT" => Some(format!("{tag} NO no such mailbox\r\n")),
            _ => session_reply(tag, command),
        })
        .await;
        let error = serve_once(&config(port, "none"), b"dev").await;
        assert_prefixed(&error, AccountDetailCode::SyncFailed, "SELECT INBOX 失败");
        assert!(error.server_detail().contains("no such mailbox"), "{error}");
    }

    #[tokio::test]
    async fn idle_failed_when_the_server_refuses_idle() {
        let port = scripted_server("* OK ready\r\n", |tag, command| match command {
            "IDLE" => Some(format!("{tag} BAD idle refused\r\n")),
            _ => session_reply(tag, command),
        })
        .await;
        let error = serve_once(&config(port, "none"), b"dev").await;
        assert_prefixed(&error, AccountDetailCode::IdleFailed, "IDLE 失败");
        assert!(error.server_detail().contains("idle refused"), "{error}");
    }

    #[tokio::test]
    async fn idle_timeout_when_idle_is_never_acknowledged() {
        let _short = ShortTimeouts::apply(|t| t.idle_init = SHORT);
        let port = scripted_server("* OK ready\r\n", session_reply).await;
        let error = serve_once(&config(port, "none"), b"dev").await;
        assert_fixed(&error, AccountDetailCode::IdleTimeout, "IDLE 启动超时");
    }

    #[tokio::test]
    async fn poll_failed_without_a_scheduler() {
        let port = scripted_server("* OK ready\r\n", session_reply).await;
        let cfg = ReceiveConfig {
            idle: false,
            ..config(port, "none")
        };
        let error = serve_once(&cfg, b"dev").await;
        assert_fixed(&error, AccountDetailCode::PollFailed, "轮询 receive 缺少 scheduler 服务");
    }

    // —— 委托收信 ——

    fn provider_spec() -> ProviderReceiveSpec {
        ProviderReceiveSpec {
            account: "acc".into(),
            address: "acc@example.test".into(),
            provider_id: "org.example.provider".into(),
            poll_secs: 60,
        }
    }

    /// 装配好 store 与注册表，注册一个端点；返回端点与它的出站帧接收端。
    fn provider_context() -> (
        Context,
        Arc<crate::provider::ProviderEndpoint>,
        mpsc::Receiver<String>,
    ) {
        let ctx = Context::new();
        ctx.provide::<Store>(Arc::new(Store::open("").unwrap()));
        let registry = crate::provider::ProviderRegistry::new();
        let (output, frames) = mpsc::channel(8);
        let endpoint = Arc::new(crate::provider::ProviderEndpoint::new(
            "org.example.provider".into(),
            output,
            crate::provider::SessionCallState::new(),
        ));
        registry.register(Arc::clone(&endpoint));
        ctx.provide::<crate::provider::ProviderRegistry>(registry);
        (ctx, endpoint, frames)
    }

    /// 回答下一帧反向调用。
    fn answer_next(
        endpoint: Arc<crate::provider::ProviderEndpoint>,
        mut frames: mpsc::Receiver<String>,
        reply: crate::provider::ProviderReply,
    ) {
        tokio::spawn(async move {
            let frame = frames.recv().await.expect("反向调用帧");
            let value: serde_json::Value = serde_json::from_str(&frame).unwrap();
            let id = value["id"].to_string();
            assert!(endpoint.calls().route(id.trim_matches('"'), reply));
            // 保持接收端存活，后续帧不至于变成「连接已关闭」。
            while frames.recv().await.is_some() {}
        });
    }

    async fn drain_once(ctx: &Context) -> ReceiveError {
        let receive = Receive::new("acc");
        tokio::time::timeout(Duration::from_secs(10), provider_drain(&provider_spec(), ctx, &receive))
            .await
            .expect("provider_drain 必须在时限内返回")
            .expect_err("provider_drain 应失败")
    }

    #[tokio::test]
    async fn internal_and_provider_unavailable_for_missing_registry_or_endpoint() {
        let error = drain_once(&Context::new()).await;
        assert_fixed(&error, AccountDetailCode::Internal, "provider 注册表未装配");

        let ctx = Context::new();
        ctx.provide::<crate::provider::ProviderRegistry>(crate::provider::ProviderRegistry::new());
        let error = drain_once(&ctx).await;
        assert_fixed(
            &error,
            AccountDetailCode::ProviderUnavailable,
            "provider org.example.provider 未注册",
        );

        // 端点的连接已断：provider 模块自己写的说明不透出。
        let (ctx, _endpoint, frames) = provider_context();
        drop(frames);
        let error = drain_once(&ctx).await;
        assert_fixed(
            &error,
            AccountDetailCode::ProviderUnavailable,
            "provider 不可用: provider 连接已关闭",
        );
    }

    #[tokio::test]
    async fn provider_timeout_when_the_plugin_never_answers() {
        let _short = ShortTimeouts::apply(|t| t.provider_call = Some(SHORT));
        let (ctx, _endpoint, _frames) = provider_context();
        let error = drain_once(&ctx).await;
        assert_fixed(&error, AccountDetailCode::ProviderTimeout, "provider 响应超时");
    }

    #[tokio::test]
    async fn provider_failed_carries_the_sanitized_plugin_message() {
        let (ctx, endpoint, frames) = provider_context();
        answer_next(
            endpoint,
            frames,
            crate::provider::ProviderReply::Err {
                code: -32020,
                message: "mailbox\nlocked".into(),
            },
        );
        let error = drain_once(&ctx).await;
        assert_eq!(error.code(), AccountDetailCode::ProviderFailed);
        assert_eq!(error.to_string(), "provider 上报失败(-32020): mailboxlocked");
        assert_eq!(error.server_detail(), "mailboxlocked");
    }

    #[tokio::test]
    async fn provider_response_invalid_for_a_malformed_batch() {
        let (ctx, endpoint, frames) = provider_context();
        answer_next(
            endpoint,
            frames,
            crate::provider::ProviderReply::Ok(serde_json::json!({"messages": [{"uid": "x1"}]})),
        );
        let error = drain_once(&ctx).await;
        assert_fixed(
            &error,
            AccountDetailCode::ProviderResponseInvalid,
            "provider receive 应答结构无效",
        );
    }

    #[test]
    fn op_failure_texts_split_into_prefix_and_server_text() {
        assert_eq!(
            op_failure_server_text("INBOX", "APPEND 失败: no response: quota"),
            Some("no response: quota")
        );
        assert_eq!(
            op_failure_server_text("Work/Old", "SELECT Work/Old 失败: no response: gone"),
            Some("no response: gone")
        );
        assert_eq!(
            op_failure_server_text("INBOX", "已复制并标记删除，EXPUNGE 失败: connection lost"),
            Some("connection lost")
        );
        for ours in ["用户放弃", "flag 载荷无法解析", "文件夹记录已丢失", "SELECT Other 失败: x"] {
            assert_eq!(op_failure_server_text("INBOX", ours), None, "{ours}");
        }
    }
}
