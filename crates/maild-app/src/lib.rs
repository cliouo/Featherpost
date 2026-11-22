//! Framework-neutral application layer for Featherpost desktop consumers.
//!
//! This crate owns reconnecting, request multiplexing and rebuildable view
//! state. It deliberately contains no egui or mail-domain implementation.

mod actor;

use std::time::Duration;

use maild_client::{ClientError, Endpoint};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;

pub use maild_client::{
    compat_from_response, AccountDraftInput, AccountProfileView, AccountSecretUpdate,
    AccountSource, AccountsResult, AccountState, CompatVerdict, CreateAccountResult,
    CredentialsView, ExternalPluginInfo, ExternalPluginsResult, FrontendInfo, FrontendsResult,
    IncomingInput, IncomingView, MailMessage, MessageResult, MessageSummariesResult,
    MessageSummary, MessagesResult, Notification, NotificationAuthorization,
    NotificationAvailability, NotificationStatus, OpenExternalPluginUiResult, OpenFrontendResult,
    OpenIntent, OpenIntentResult,
    OutgoingInput, OutgoingView, ProbeOutcomeView, ProbeReportView, ProfilesResult, QuietHours,
    RemoveAccountResult, RulesPatch, RulesSettings, RuntimeView, SendRequest, ServerNotification,
    SettingsResult, StatusResult, UpdatePatchInput, DEFAULT_FRONTEND,
};

const COMMAND_CAPACITY: usize = 64;
const EVENT_CAPACITY: usize = 128;
const MIN_TIMER_DURATION: Duration = Duration::from_millis(10);
const MIN_RECONNECT_DELAY: Duration = Duration::from_millis(100);
const MIN_HTTP_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportKind {
    Http,
    Unix,
    NamedPipe,
}

impl TransportKind {
    fn from_endpoint(endpoint: &Endpoint) -> Self {
        match endpoint {
            Endpoint::Http { .. } => Self::Http,
            Endpoint::Unix { .. } => Self::Unix,
            Endpoint::NamedPipe { .. } => Self::NamedPipe,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionPhase {
    Disconnected,
    Connecting { attempt: u32 },
    Synchronizing,
    Online,
    RetryWaiting { attempt: u32, delay: Duration },
    Stopped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewState {
    pub transport: TransportKind,
    pub phase: ConnectionPhase,
    /// Monotonically increases after each newly established connection has
    /// produced its first authoritative snapshot.
    pub connection_epoch: u64,
    pub version: Option<String>,
    pub accounts: Vec<AccountState>,
    pub messages: Vec<MessageSummary>,
    pub total_messages: i64,
    pub partial_snapshot: bool,
    pub last_error: Option<String>,
    pub revision: u64,
    /// 当前连接绑定的主体（ADR-0030）。未声明身份或尚未绑定时为 `None`；
    /// 绑定成功后是 `whoami` 回报的 `frontend:<id>`，不是本地拼出来的。
    pub principal: Option<String>,
    /// 最近一次身份 bootstrap 的失败原因；成功即清空。与 `last_error` 分开，
    /// 因为它的修复动作不同——是「去安装/修复前端包」，不是「等网络」。
    pub identity_failure: Option<String>,
    /// ADR-0045：收到发给本前端的 `frontend/intent` 唤醒推送的次数，单调递增、跨重连保留。
    /// 壳观察到它变大就把（可能隐藏在菜单栏的）窗口带回前台并去 claim 意图。
    pub intent_epoch: u64,
    /// ADR-0046：daemon 推来了 `agent/stopping`（菜单栏「退出 <产品名>」）。一旦置位不再回落——
    /// 壳观察到它变成 `true` 就走与关闭窗口同一条优雅退出路径，随后台一起离场。
    pub shutdown_requested: bool,
    /// ADR-0048：daemon 解析后的有序语言偏好（`effective`）。每次连接的首个快照经
    /// `getLocalePreference` 取得，之后随 `preferences/locale` 推送更新；跨重连保留上一次的值。
    /// `None` = 还没取到，或 daemon 不提供这个方法（老 daemon 的 `-32601`、不认识它的
    /// 授权表给的 `-32016`）——前端此时回落基础语言，即今天的行为。
    pub locale: Option<Vec<String>>,
}

impl ViewState {
    fn new(transport: TransportKind) -> Self {
        Self {
            transport,
            phase: ConnectionPhase::Disconnected,
            connection_epoch: 0,
            version: None,
            accounts: Vec::new(),
            messages: Vec::new(),
            total_messages: 0,
            partial_snapshot: false,
            last_error: None,
            revision: 0,
            principal: None,
            identity_failure: None,
            intent_epoch: 0,
            shutdown_requested: false,
            locale: None,
        }
    }
}

/// 以前端主体连接（ADR-0030 决策 6，ADR-0031 决策 3）。
///
/// actor 在每次建立 IPC 连接后、发第一个快照请求前：用一条短命的 user 连接
/// `revealPluginToken`（reveal 即轮换），在长连接上 `authenticate`，再以 `whoami` 校验。
/// 三步任一失败按连接失败退避重试——用户可能正在安装包。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendIdentity {
    /// frontend package id，必须等于受管目录名。
    pub frontend_id: String,
}

const MAX_FRONTEND_ID_BYTES: usize = 128;

impl FrontendIdentity {
    fn validate(&self) -> Result<(), ClientError> {
        let id = &self.frontend_id;
        if id.is_empty() || id.len() > MAX_FRONTEND_ID_BYTES {
            return Err(ClientError::connection("frontend_id 必须是 1 到 128 字节"));
        }
        if !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return Err(ClientError::connection(
                "frontend_id 只能包含字母、数字、`.`、`-`、`_`",
            ));
        }
        Ok(())
    }

    /// `whoami` 应回报的主体字符串。
    pub fn expected_principal(&self) -> String {
        format!("frontend:{}", self.frontend_id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AppEvent {
    Notification(ServerNotification),
}

#[derive(Clone, Debug)]
pub struct ReconnectPolicy {
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl ReconnectPolicy {
    fn validate(&self) -> Result<(), ClientError> {
        if self.initial_delay < MIN_RECONNECT_DELAY {
            return Err(ClientError::connection(
                "reconnect initial_delay 不能小于 100ms",
            ));
        }
        if self.max_delay < self.initial_delay {
            return Err(ClientError::connection(
                "reconnect max_delay 不能小于 initial_delay",
            ));
        }
        if self.max_delay > MAX_RECONNECT_DELAY {
            return Err(ClientError::connection(
                "reconnect max_delay 不能大于 5 分钟",
            ));
        }
        Ok(())
    }

    fn delay(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(31);
        self.initial_delay
            .checked_mul(1_u32 << exponent)
            .unwrap_or(self.max_delay)
            .min(self.max_delay)
    }
}

#[derive(Clone, Debug)]
pub struct ActorOptions {
    pub initial_page_size: u64,
    pub request_timeout: Duration,
    pub http_poll_interval: Duration,
    pub reconnect: ReconnectPolicy,
    /// `None` = 以 user 主体连接（旧桌面壳、Menu 的现状）。
    pub identity: Option<FrontendIdentity>,
}

impl Default for ActorOptions {
    fn default() -> Self {
        Self {
            initial_page_size: 50,
            request_timeout: Duration::from_secs(30),
            http_poll_interval: Duration::from_secs(30),
            reconnect: ReconnectPolicy::default(),
            identity: None,
        }
    }
}

impl ActorOptions {
    fn validate(&self) -> Result<(), ClientError> {
        if let Some(identity) = &self.identity {
            identity.validate()?;
        }
        if !(1..=500).contains(&self.initial_page_size) {
            return Err(ClientError::connection("initial_page_size 必须是 1 到 500"));
        }
        if self.request_timeout < MIN_TIMER_DURATION {
            return Err(ClientError::connection("request_timeout 不能小于 10ms"));
        }
        if self.request_timeout > MAX_REQUEST_TIMEOUT {
            return Err(ClientError::connection("request_timeout 不能大于 5 分钟"));
        }
        if self.http_poll_interval < MIN_HTTP_POLL_INTERVAL {
            return Err(ClientError::connection("http_poll_interval 不能小于 1 秒"));
        }
        if self.http_poll_interval > MAX_POLL_INTERVAL {
            return Err(ClientError::connection(
                "http_poll_interval 不能大于 24 小时",
            ));
        }
        self.reconnect.validate()
    }
}

pub struct ConnectionHandle {
    commands: mpsc::Sender<actor::Command>,
    state: watch::Receiver<ViewState>,
    events: broadcast::Receiver<AppEvent>,
    shutdown: watch::Sender<bool>,
    request_timeout: Duration,
}

impl Clone for ConnectionHandle {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            state: self.state.clone(),
            events: self.events.resubscribe(),
            shutdown: self.shutdown.clone(),
            request_timeout: self.request_timeout,
        }
    }
}

impl ConnectionHandle {
    pub fn spawn(endpoint: Endpoint, options: ActorOptions) -> Result<Self, ClientError> {
        options.validate()?;
        // authenticate 只接受同用户 IPC（ADR-0017）；HTTP 会话恒为 user。一个声称要以前端
        // 主体运行的壳不该静默降级成 user——那正是 ADR-0030 要消灭的形态。
        if options.identity.is_some() && !endpoint.supports_notifications() {
            return Err(ClientError::connection(
                "前端主体只能经同用户 IPC（Unix socket / named pipe）绑定；HTTP 端点恒为 user 主体",
            ));
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| ClientError::connection("连接 actor 需要 Tokio runtime"))?;
        let transport = TransportKind::from_endpoint(&endpoint);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (state_tx, state_rx) = watch::channel(ViewState::new(transport));
        let (event_tx, event_rx) = broadcast::channel(EVENT_CAPACITY);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let client = maild_client::RpcClient::new(endpoint)?;
        let request_timeout = options.request_timeout;
        runtime.spawn(actor::run(
            client,
            options,
            command_rx,
            state_tx,
            event_tx,
            shutdown_rx,
        ));
        Ok(Self {
            commands: command_tx,
            state: state_rx,
            events: event_rx,
            shutdown: shutdown_tx,
            request_timeout,
        })
    }

    pub fn state(&self) -> ViewState {
        self.state.borrow().clone()
    }

    pub fn subscribe_state(&self) -> watch::Receiver<ViewState> {
        self.state.clone()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<AppEvent> {
        self.events.resubscribe()
    }

    pub async fn request(
        &self,
        method: impl Into<String>,
        params: Value,
    ) -> Result<Value, ClientError> {
        if self.state.borrow().phase != ConnectionPhase::Online {
            return Err(ClientError::connection("连接尚未就绪"));
        }
        let deadline = Instant::now() + self.request_timeout;
        let (reply_tx, reply_rx) = oneshot::channel();
        tokio::time::timeout_at(
            deadline,
            self.commands.send(actor::Command::Request {
                method: method.into(),
                params,
                deadline,
                reply: reply_tx,
            }),
        )
        .await
        .map_err(|_| ClientError::connection("JSON-RPC 请求排队超时"))?
        .map_err(|_| ClientError::connection("连接 actor 已停止"))?;
        tokio::time::timeout_at(deadline, reply_rx)
            .await
            .map_err(|_| ClientError::connection("JSON-RPC 请求超时"))?
            .map_err(|_| ClientError::connection("连接 actor 未返回请求结果"))?
    }

    pub async fn request_typed<T>(
        &self,
        method: impl Into<String>,
        params: Value,
    ) -> Result<T, ClientError>
    where
        T: DeserializeOwned,
    {
        let method = method.into();
        let value = self.request(method.clone(), params).await?;
        serde_json::from_value(value)
            .map_err(|error| ClientError::rpc(format!("{method} 响应结构无效: {error}")))
    }

    pub async fn message(&self, account: &str, uid: &str) -> Result<MessageResult, ClientError> {
        self.request_typed("getMessage", json!({ "account": account, "uid": uid }))
            .await
    }

    pub async fn message_summaries(
        &self,
        account: Option<&str>,
        limit: u64,
        offset: u64,
    ) -> Result<MessageSummariesResult, ClientError> {
        self.request_typed(
            "listMessageSummaries",
            json!({ "account": account, "limit": limit, "offset": offset }),
        )
        .await
    }

    pub async fn settings(&self) -> Result<SettingsResult, ClientError> {
        let result: SettingsResult = self.request_typed("getSettings", json!({})).await?;
        maild_client::validate_settings_result(&result, false)
            .map_err(|error| ClientError::rpc(format!("getSettings 响应无效: {error}")))?;
        Ok(result)
    }

    pub async fn notification_status(&self) -> Result<NotificationStatus, ClientError> {
        self.request_typed("getNotificationStatus", json!({})).await
    }

    pub async fn request_notification_authorization(
        &self,
    ) -> Result<NotificationStatus, ClientError> {
        self.request_typed("requestNotificationAuthorization", json!({}))
            .await
    }

    pub async fn claim_next_open_intent(
        &self,
        frontend: &str,
    ) -> Result<OpenIntentResult, ClientError> {
        self.request_typed("claimNextOpenIntent", json!({ "frontend": frontend }))
            .await
    }

    pub async fn list_external_plugins(&self) -> Result<ExternalPluginsResult, ClientError> {
        self.request_typed("listExternalPlugins", json!({})).await
    }

    pub async fn open_external_plugin_ui(
        &self,
        id: &str,
    ) -> Result<OpenExternalPluginUiResult, ClientError> {
        self.request_typed("openExternalPluginUi", json!({ "id": id }))
            .await
    }

    pub async fn commit_plugin_install(
        &self,
        staging: &str,
    ) -> Result<maild_client::CommitPluginInstallResult, ClientError> {
        self.request_typed("commitPluginInstall", json!({ "staging": staging }))
            .await
    }

    pub async fn set_plugin_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<maild_client::SetPluginEnabledResult, ClientError> {
        self.request_typed("setPluginEnabled", json!({ "id": id, "enabled": enabled }))
            .await
    }

    pub async fn set_settings(&self, patch: &RulesPatch) -> Result<SettingsResult, ClientError> {
        maild_client::validate_rules_patch(patch)
            .map_err(|error| ClientError::rpc(format!("rules 设置无效: {error}")))?;
        let result: SettingsResult = self
            .request_typed("setSettings", json!({ "rules": patch }))
            .await?;
        maild_client::validate_settings_result(&result, true)
            .map_err(|error| ClientError::rpc(format!("setSettings 响应无效: {error}")))?;
        Ok(result)
    }

    pub async fn send(&self, message: &SendRequest) -> Result<(), ClientError> {
        maild_client::validate_send_request(message)
            .map_err(|error| ClientError::rpc(error.to_string()))?;
        let result = self.request("send", json!(message)).await?;
        if result.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(ClientError::rpc("send 响应缺少 ok=true"))
        }
    }

    /// 版本握手（ADR-0016）：判定 + 遗留内核降级共享规则见 `compat_from_response`。
    pub async fn agent_compat(&self, required: u64) -> Result<CompatVerdict, ClientError> {
        let response = self
            .request_typed::<maild_client::AgentInfo>("getAgentInfo", json!({}))
            .await;
        compat_from_response(required, response)
    }

    /// —— M3.7 账户管理（ADR-0015 §9）：秘密只写不读，桌面端不落盘 ——
    pub async fn list_account_profiles(
        &self,
        include_archived: bool,
    ) -> Result<ProfilesResult, ClientError> {
        self.request_typed(
            "listAccountProfiles",
            json!({ "include_archived": include_archived }),
        )
        .await
    }

    // —— 地址族（M4-06，ADR-0022） ——

    pub async fn list_addresses(
        &self,
        account: Option<&str>,
        include_inactive: bool,
    ) -> Result<Vec<maild_client::AddressView>, ClientError> {
        let mut payload = json!({ "include_inactive": include_inactive });
        if let Some(account) = account {
            payload["account"] = json!(account);
        }
        let wrapped: maild_client::AddressesResult =
            self.request_typed("listAddresses", payload).await?;
        Ok(wrapped.addresses)
    }

    pub async fn list_addresses_page(
        &self,
        account: Option<&str>,
        include_inactive: bool,
        limit: u64,
        offset: u64,
    ) -> Result<maild_client::AddressesResult, ClientError> {
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
        self.request_typed("listAddresses", payload).await
    }

    pub async fn create_address(
        &self,
        account: &str,
        kind: &str,
        address: Option<&str>,
        label: Option<&str>,
        expires_secs: Option<u64>,
    ) -> Result<maild_client::AddressView, ClientError> {
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
        self.request_typed("createAddress", payload).await
    }

    pub async fn update_address(
        &self,
        id: &str,
        state: Option<&str>,
        label: Option<&str>,
    ) -> Result<maild_client::AddressView, ClientError> {
        let mut payload = json!({ "id": id });
        if let Some(state) = state {
            payload["state"] = json!(state);
        }
        if let Some(label) = label {
            payload["label"] = json!(label);
        }
        self.request_typed("updateAddress", payload).await
    }

    pub async fn remove_address(&self, id: &str) -> Result<(), ClientError> {
        #[derive(serde::Deserialize)]
        struct RemoveResult {
            ok: bool,
        }
        let result: RemoveResult = self
            .request_typed("removeAddress", json!({ "id": id }))
            .await?;
        if result.ok {
            Ok(())
        } else {
            Err(ClientError::rpc("removeAddress 响应缺少 ok=true"))
        }
    }

    pub async fn probe_account(
        &self,
        draft: &AccountDraftInput,
        incoming_secret: &str,
        outgoing_secret: Option<&str>,
    ) -> Result<ProbeReportView, ClientError> {
        let params = account_probe_params(draft, incoming_secret, outgoing_secret)?;
        self.request_typed("probeAccount", params).await
    }

    pub async fn create_account(
        &self,
        draft: &AccountDraftInput,
        incoming_secret: &str,
        outgoing_secret: Option<&str>,
    ) -> Result<CreateAccountResult, ClientError> {
        let params = account_probe_params(draft, incoming_secret, outgoing_secret)?;
        self.request_typed("createAccount", params).await
    }

    pub async fn update_account(
        &self,
        id: &str,
        expected_revision: u64,
        patch: &UpdatePatchInput,
        secrets: AccountSecretUpdate<'_>,
    ) -> Result<AccountProfileView, ClientError> {
        if id.is_empty() || expected_revision < 1 {
            return Err(ClientError::rpc("id/expected_revision 无效"));
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
                    credentials["incoming"] = json!(secret);
                }
                if let Some(secret) = secrets.outgoing {
                    credentials["outgoing"] = json!(secret);
                }
                params["credentials"] = credentials;
            }
        }
        self.request_typed("updateAccount", params).await
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
        self.request_typed(
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
        self.request_typed(
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
        #[derive(serde::Deserialize)]
        struct SendResult {
            ok: bool,
        }
        if account.is_empty() {
            return Err(ClientError::rpc("account 不能为空"));
        }
        maild_client::validate_send_field(maild_client::SendField::To, to)
            .map_err(|error| ClientError::rpc(error.to_string()))?;
        maild_client::validate_send_field(maild_client::SendField::Subject, subject)
            .map_err(|error| ClientError::rpc(error.to_string()))?;
        maild_client::validate_send_field(maild_client::SendField::Body, body)
            .map_err(|error| ClientError::rpc(error.to_string()))?;
        let payload = serde_json::to_value(maild_client::SendFromAccountRequest {
            account: account.into(),
            to: to.into(),
            subject: subject.into(),
            body: body.into(),
            from_address: from_address.map(str::to_owned),
        })
        .map_err(|_| ClientError::rpc("sendFromAccount 请求序列化失败"))?;
        let result: SendResult = self.request_typed("sendFromAccount", payload).await?;
        if result.ok {
            Ok(())
        } else {
            Err(ClientError::rpc("sendFromAccount 响应缺少 ok=true"))
        }
    }

    pub async fn refresh(&self) -> Result<(), ClientError> {
        if self.state.borrow().phase != ConnectionPhase::Online {
            return Err(ClientError::connection("连接尚未就绪"));
        }
        let deadline = Instant::now() + self.request_timeout;
        let (reply_tx, reply_rx) = oneshot::channel();
        tokio::time::timeout_at(
            deadline,
            self.commands.send(actor::Command::Refresh {
                deadline,
                reply: reply_tx,
            }),
        )
        .await
        .map_err(|_| ClientError::connection("刷新请求排队超时"))?
        .map_err(|_| ClientError::connection("连接 actor 已停止"))?;
        tokio::time::timeout_at(deadline, reply_rx)
            .await
            .map_err(|_| ClientError::connection("刷新请求超时"))?
            .map_err(|_| ClientError::connection("连接 actor 未返回刷新结果"))?
    }

    pub async fn shutdown(&self) -> Result<(), ClientError> {
        self.shutdown.send_replace(true);
        let deadline = Instant::now() + self.request_timeout;
        let mut state = self.state.clone();
        tokio::time::timeout_at(deadline, async move {
            loop {
                if state.borrow().phase == ConnectionPhase::Stopped {
                    return Ok(());
                }
                state
                    .changed()
                    .await
                    .map_err(|_| ClientError::connection("连接 actor 未发布 Stopped 就已退出"))?;
            }
        })
        .await
        .map_err(|_| ClientError::connection("连接 actor 关闭超时"))?
    }
}

/// 组装 probe/create 共用参数；秘密校验复用客户端规则。
fn account_probe_params(
    draft: &AccountDraftInput,
    incoming_secret: &str,
    outgoing_secret: Option<&str>,
) -> Result<Value, ClientError> {
    maild_client::validate_draft_input(draft)
        .map_err(|error| ClientError::rpc(error.to_string()))?;
    let credentials =
        maild_client::account_credentials_input(draft, incoming_secret, outgoing_secret)?;
    Ok(json!({ "draft": draft, "credentials": credentials }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_backoff_is_exponential_and_capped() {
        let policy = ReconnectPolicy {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(350),
        };
        assert_eq!(policy.delay(1), Duration::from_millis(100));
        assert_eq!(policy.delay(2), Duration::from_millis(200));
        assert_eq!(policy.delay(3), Duration::from_millis(350));
        assert_eq!(policy.delay(30), Duration::from_millis(350));
    }

    #[test]
    fn actor_options_reject_invalid_resource_bounds() {
        assert!(ActorOptions {
            initial_page_size: 0,
            ..ActorOptions::default()
        }
        .validate()
        .is_err());
        assert!(ActorOptions {
            initial_page_size: 501,
            ..ActorOptions::default()
        }
        .validate()
        .is_err());
        assert!(ActorOptions {
            request_timeout: Duration::ZERO,
            ..ActorOptions::default()
        }
        .validate()
        .is_err());
        assert!(ActorOptions {
            request_timeout: Duration::MAX,
            ..ActorOptions::default()
        }
        .validate()
        .is_err());
        assert!(ActorOptions {
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_secs(1),
            },
            ..ActorOptions::default()
        }
        .validate()
        .is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn frontend_identity_requires_an_ipc_endpoint() {
        let error = ConnectionHandle::spawn(
            Endpoint::Http {
                url: "http://127.0.0.1:9/rpc".into(),
                token: "t".repeat(32),
            },
            ActorOptions {
                identity: Some(FrontendIdentity {
                    frontend_id: "com.example.shell".into(),
                }),
                ..ActorOptions::default()
            },
        )
        .err()
        .expect("HTTP + identity must be refused at spawn");
        assert!(error.message.contains("IPC"), "{}", error.message);

        for bad in ["", "has space", "über"] {
            assert!(
                FrontendIdentity {
                    frontend_id: bad.into(),
                }
                .validate()
                .is_err(),
                "{bad:?} 应被拒绝"
            );
        }
        assert_eq!(
            FrontendIdentity {
                frontend_id: maild_brand::BUNDLED_FRONTEND_ID.into(),
            }
            .expected_principal(),
            format!("frontend:{}", maild_brand::BUNDLED_FRONTEND_ID)
        );
    }

    #[test]
    fn spawning_without_tokio_runtime_is_rejected() {
        let error = ConnectionHandle::spawn(
            Endpoint::Http {
                url: "http://127.0.0.1:7394/rpc".into(),
                token: "t".repeat(32),
            },
            ActorOptions::default(),
        )
        .err()
        .expect("spawn outside Tokio must fail");
        assert!(error.message.contains("Tokio runtime"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn requests_fail_fast_while_connection_is_offline() {
        let handle = ConnectionHandle::spawn(
            Endpoint::Http {
                url: "http://127.0.0.1:9/rpc".into(),
                token: "t".repeat(32),
            },
            ActorOptions {
                request_timeout: Duration::from_millis(100),
                ..ActorOptions::default()
            },
        )
        .unwrap();
        let mut events = handle.subscribe_events();
        let error = tokio::time::timeout(
            Duration::from_millis(20),
            handle.request("status", json!({})),
        )
        .await
        .expect("offline request should not wait for connect timeout")
        .unwrap_err();
        assert!(error.message.contains("尚未就绪"));
        handle.shutdown().await.unwrap();
        assert_eq!(handle.state().phase, ConnectionPhase::Stopped);
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(100), events.recv())
                .await
                .unwrap(),
            Err(broadcast::error::RecvError::Closed)
        ));
    }
}
