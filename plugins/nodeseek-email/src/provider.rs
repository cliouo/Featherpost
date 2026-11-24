use std::collections::VecDeque;
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use maild_client::{
    DiscoveryOptions, Endpoint, Incoming, JsonRpcError, ProxyConfigResult, StreamReader,
    StreamWriter, TransportProbeRequest, TransportReceiveRequest, TransportSendRequest,
};
use maild_proxy::{ProxyConfig, SystemProxySnapshot};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::time::Instant;
use zeroize::Zeroizing;

use crate::api::ApiClient;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::proxy::{
    convert_client_proxy_result, convert_proxy_event, event_requires_secret_fetch,
    PluginProxyStore, ProxySnapshot,
};
use crate::secret::{PlatformSecretStore, SecretStore as _};
use crate::state::{canonicalize_address, MailboxBinding, StateStore};

const INVALID_PARAMS: i64 = -32602;
const METHOD_NOT_FOUND: i64 = -32601;
const PROVIDER_ERROR: i64 = -32050;
const DELIVERY_UNKNOWN: i64 = -32051;
const RATE_LIMITED: i64 = -32052;
const RECEIVE_OPERATION_TIMEOUT: Duration = Duration::from_secs(40);
const RECEIVE_RETURN_RESERVE: Duration = Duration::from_secs(8);
const PROXY_IPC_FETCH_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_DEFERRED_FRAMES: usize = 16;

pub struct ProviderRuntime {
    api: ApiClient,
    state: StateStore,
    proxy_store: Option<PluginProxyStore>,
    last_override_stamp: Mutex<Option<(SystemTime, u64)>>,
}

impl ProviderRuntime {
    pub fn production() -> AppResult<Self> {
        let secret = PlatformSecretStore.load()?;
        let state = StateStore::discover()?;
        let proxy_store = PluginProxyStore::discover().ok();
        let last_stamp = proxy_store.as_ref().and_then(PluginProxyStore::file_stamp);
        let plugin_override = proxy_store
            .as_ref()
            .and_then(|store| store.load().ok())
            .map(|settings| settings.proxy_override)
            .unwrap_or_default();
        let proxy_snapshot = ProxySnapshot::default_system().with_override(plugin_override);
        Ok(Self {
            api: ApiClient::production_with_proxy(secret, proxy_snapshot)?,
            state,
            proxy_store,
            last_override_stamp: Mutex::new(last_stamp),
        })
    }

    #[cfg(any(test, debug_assertions))]
    pub fn for_test(
        api: ApiClient,
        state: StateStore,
        proxy_store: Option<PluginProxyStore>,
    ) -> Self {
        let last_stamp = proxy_store.as_ref().and_then(PluginProxyStore::file_stamp);
        Self {
            api,
            state,
            proxy_store,
            last_override_stamp: Mutex::new(last_stamp),
        }
    }

    /// Hot-reload inherited global proxy configuration while preserving any plugin-level override.
    pub fn update_global_proxy(&self, config: ProxyConfig, system: SystemProxySnapshot) {
        if let Err(error) = self.api.update_global_proxy(config, system) {
            eprintln!("nodeseek-email-provider: 更新全局代理失败: {error}");
        }
    }

    /// Check if `proxy_override.json` was updated externally (e.g. from UI or CLI) and reload it.
    fn refresh_local_override(&self) {
        let Some(store) = &self.proxy_store else {
            return;
        };
        let stamp = store.file_stamp();
        let mut last = self
            .last_override_stamp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *last == stamp {
            return;
        }
        *last = stamp;
        drop(last);
        if let Ok(settings) = store.load() {
            if let Err(error) = self.api.update_plugin_override(settings.proxy_override) {
                eprintln!("nodeseek-email-provider: 更新插件代理覆盖失败: {error}");
            }
        }
    }

    pub async fn handle(&self, method: &str, params: Value) -> Result<Value, JsonRpcError> {
        self.refresh_local_override();
        match method {
            "transport/probe" => self.probe(parse_params(params)?).await,
            "transport/receive" => self.receive(parse_params(params)?).await,
            "transport/send" => self.send(parse_params(params)?).await,
            _ => Err(JsonRpcError {
                code: METHOD_NOT_FOUND,
                message: "provider 方法不存在".into(),
                data: None,
            }),
        }
    }

    async fn probe(&self, params: ProbeParams) -> Result<Value, JsonRpcError> {
        if !matches!(params.direction.as_str(), "incoming" | "outgoing") {
            return Err(invalid_params());
        }
        let mailbox = if params.account.is_empty() {
            let address = canonicalize_address(&params.address).map_err(provider_error)?;
            if self
                .state
                .binding_for_address(&address)
                .map_err(provider_error)?
                .is_some()
            {
                return Err(provider_error(AppError::new(
                    ErrorCode::Conflict,
                    "远端邮箱仍绑定到另一个本地账户；确认旧账户已归档后先解除本地导入",
                )));
            }
            self.api
                .resolve_mailbox(&address)
                .await
                .map_err(provider_error)?
        } else {
            self.probe_existing_mailbox(&params.account, &params.address)
                .await
                .map_err(provider_error)?
        };
        if params.direction == "outgoing" && mailbox.send_disabled == Some(true) {
            return Err(provider_error(AppError::new(
                ErrorCode::Forbidden,
                "该 NodeSeek Email 邮箱已禁用发信",
            )));
        }
        Ok(json!({
            "ok": true,
            "stage": "provider",
            "detail": "NodeSeek Email provider 可用"
        }))
    }

    async fn receive(&self, params: ReceiveParams) -> Result<Value, JsonRpcError> {
        self.receive_with_budget(params, RECEIVE_OPERATION_TIMEOUT, RECEIVE_RETURN_RESERVE)
            .await
    }

    async fn receive_with_budget(
        &self,
        params: ReceiveParams,
        operation_timeout: Duration,
        return_reserve: Duration,
    ) -> Result<Value, JsonRpcError> {
        if operation_timeout.is_zero() || return_reserve >= operation_timeout {
            return Err(provider_error(receive_timeout()));
        }
        let started = Instant::now();
        let hard_deadline = started + operation_timeout;
        let work_deadline = hard_deadline - return_reserve;
        tokio::time::timeout_at(
            hard_deadline,
            self.receive_before_deadline(params, work_deadline),
        )
        .await
        .unwrap_or_else(|_| Err(provider_error(receive_timeout())))
    }

    async fn receive_before_deadline(
        &self,
        params: ReceiveParams,
        work_deadline: Instant,
    ) -> Result<Value, JsonRpcError> {
        let since = parse_cursor(&params.since_uid)?;
        let max = usize::try_from(params.max).map_err(|_| invalid_params())?;
        if max == 0 || max > 20 {
            return Err(invalid_params());
        }
        let binding = before_receive_deadline(
            work_deadline,
            self.checked_binding(&params.account, &params.address),
        )
        .await
        .map_err(provider_error)?;
        let mut pending = self
            .state
            .pending(&params.account, since, max)
            .map_err(provider_error)?;
        // Drain already-discovered mail before spending a request on another list page. When the
        // queue is empty, advance exactly one persisted discovery page so large inboxes make
        // progress across polls without exhausting the shared rate budget before details begin.
        if pending.is_empty() {
            let offset = self
                .state
                .scan_offset(&params.account)
                .map_err(provider_error)?;
            let page = before_receive_deadline(
                work_deadline,
                self.api.remote_mail_ids_page(binding.mailbox_id, offset),
            )
            .await
            .map_err(provider_error)?;
            self.state
                .assign_remote_ids(&params.account, &page.ids)
                .map_err(provider_error)?;
            self.state
                .set_scan_offset(&params.account, page.next_offset)
                .map_err(provider_error)?;
            pending = self
                .state
                .pending(&params.account, since, max)
                .map_err(provider_error)?;
        }

        let mut messages = Vec::with_capacity(pending.len());
        for pending_mail in pending {
            let remote = match before_receive_deadline(
                work_deadline,
                self.api
                    .mail(binding.mailbox_id, pending_mail.remote_mail_id),
            )
            .await
            {
                Ok(remote) => remote,
                Err(error) if error.code() == ErrorCode::NotFound => {
                    self.state
                        .mark_remote_missing(&params.account, pending_mail.remote_mail_id)
                        .map_err(provider_error)?;
                    continue;
                }
                Err(error) if should_return_partial(&error, messages.len()) => break,
                Err(error) => return Err(provider_error(error)),
            };
            let item = json!({
                "uid": pending_mail.synthetic_uid.to_string(),
                "sender": remote.sender,
                "recipient": remote.recipient,
                "subject": remote.subject,
                "body": remote.body_text,
                "received_at": remote.received_at,
            });
            messages.push(item);
            let candidate = json!({ "messages": messages });
            let encoded = serde_json::to_vec(&candidate).map_err(|_| {
                provider_error(AppError::new(
                    ErrorCode::InvalidResponse,
                    "无法编码 provider 接收响应",
                ))
            })?;
            if encoded.len() >= maild_client::MAX_FRAME_BYTES.saturating_sub(1024) {
                messages.pop();
                if messages.is_empty() {
                    return Err(provider_error(AppError::new(
                        ErrorCode::ResponseTooLarge,
                        "单封邮件超过 provider 响应安全上限",
                    )));
                }
                break;
            }
        }
        Ok(json!({ "messages": messages }))
    }

    async fn send(&self, params: SendParams) -> Result<Value, JsonRpcError> {
        ensure_primary_from(&params.from, &params.account_address).map_err(provider_error)?;
        let binding = self
            .checked_binding(&params.account, &params.account_address)
            .await
            .map_err(provider_error)?;
        let mailbox = self
            .checked_remote_mailbox(&binding)
            .await
            .map_err(provider_error)?;
        if mailbox.send_disabled == Some(true) {
            return Err(provider_error(AppError::new(
                ErrorCode::Forbidden,
                "该 NodeSeek Email 邮箱已禁用发信",
            )));
        }
        self.api
            .send_plain_text(
                binding.mailbox_id,
                &params.to,
                &params.subject,
                &params.body,
            )
            .await
            .map_err(provider_error)?;
        Ok(json!({ "ok": true }))
    }

    async fn checked_binding(&self, account: &str, address: &str) -> AppResult<MailboxBinding> {
        let address = canonicalize_address(address)?;
        let binding = match self.state.binding(account) {
            Ok(binding) => binding,
            Err(error) if error.code() == ErrorCode::NotFound => {
                let mailbox = self.api.resolve_mailbox(&address).await?;
                self.state
                    .import_mailbox(account, &mailbox.address, mailbox.id)?
            }
            Err(error) => return Err(error),
        };
        if binding.address != address {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "宿主账户地址与导入的远端邮箱不一致",
            ));
        }
        Ok(binding)
    }

    /// Probing never imports a binding. Receive/send still establish it after the host has saved
    /// the account; a discarded editor must not reserve this remote mailbox in local state.
    async fn probe_existing_mailbox(&self, account: &str, address: &str) -> AppResult<crate::api::Mailbox> {
        let address=canonicalize_address(address)?;
        match self.state.binding(account) {
            Ok(binding) => {
                if binding.address != address { return Err(AppError::new(ErrorCode::Conflict,"宿主账户地址与导入的远端邮箱不一致")); }
                self.checked_remote_mailbox(&binding).await
            }
            Err(error) if error.code()==ErrorCode::NotFound => {
                if self.state.binding_for_address(&address)?.is_some() {
                    return Err(AppError::new(ErrorCode::Conflict,"远端邮箱仍绑定到另一个本地账户"));
                }
                self.api.resolve_mailbox(&address).await
            }
            Err(error)=>Err(error),
        }
    }

    async fn checked_remote_mailbox(
        &self,
        binding: &MailboxBinding,
    ) -> AppResult<crate::api::Mailbox> {
        let mailbox = self.api.mailbox(binding.mailbox_id).await?;
        if mailbox.address != binding.address {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "远端邮箱身份与本地导入映射不一致",
            ));
        }
        Ok(mailbox)
    }
}

fn ensure_primary_from(from: &str, account_address: &str) -> AppResult<()> {
    if from == account_address {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::Forbidden,
            "发件地址必须等于账户主地址",
        ))
    }
}

fn should_return_partial(error: &AppError, completed: usize) -> bool {
    completed > 0
        && matches!(
            error.code(),
            ErrorCode::RateLimited | ErrorCode::Network | ErrorCode::Timeout
        )
}

async fn before_receive_deadline<T>(
    deadline: Instant,
    future: impl Future<Output = AppResult<T>>,
) -> AppResult<T> {
    tokio::time::timeout_at(deadline, future)
        .await
        .unwrap_or_else(|_| Err(receive_timeout()))
}

const fn receive_timeout() -> AppError {
    AppError::new(ErrorCode::Timeout, "NodeSeek Email 接收操作已到达安全时限")
}

pub async fn run() -> AppResult<()> {
    let runtime = ProviderRuntime::production()?;
    let token = std::env::var(maild_brand::PLUGIN_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .map(Zeroizing::new)
        .ok_or_else(|| AppError::new(ErrorCode::CredentialMissing, "缺少宿主提供的插件认证令牌"))?;
    let endpoint = maild_client::discover_endpoint(&DiscoveryOptions::default())
        .map_err(|_| AppError::new(ErrorCode::Network, "无法发现宿主插件 IPC endpoint"))?;
    let outcome = run_with_runtime(runtime, endpoint, &token).await;
    drop(token);
    outcome
}

pub async fn run_with_runtime(
    runtime: ProviderRuntime,
    endpoint: Endpoint,
    token: &str,
) -> AppResult<()> {
    if !endpoint.supports_notifications() {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "transport provider 需要流式 IPC endpoint",
        ));
    }
    let mut connection = maild_client::connect_stream(&endpoint)
        .await
        .map_err(|_| AppError::new(ErrorCode::Network, "无法连接宿主插件 IPC"))?;
    connection
        .authenticate(token)
        .await
        .map_err(|_| AppError::new(ErrorCode::Authentication, "宿主插件认证失败"))?;
    connection
        .call("providerRegister", json!({}), |_| {})
        .await
        .map_err(|_| AppError::new(ErrorCode::Upstream, "宿主 provider 注册失败"))?;

    // Fetch initial global proxy configuration (including optional auth password) over local
    // stream IPC. Older daemons without `getProxyConfig` fall back cleanly to system defaults.
    if let Ok(value) = connection
        .call_with_timeout(
            "getProxyConfig",
            json!({ "include_secret": true }),
            PROXY_IPC_FETCH_TIMEOUT,
            |_| {},
        )
        .await
    {
        if let Ok(result) = serde_json::from_value::<ProxyConfigResult>(value) {
            let (cfg, sys) = convert_client_proxy_result(&result);
            runtime.update_global_proxy(cfg, sys);
        }
    }

    let (mut reader, mut writer) = connection.into_split();
    let mut deferred: VecDeque<Incoming> = VecDeque::new();
    loop {
        let next = match deferred.pop_front() {
            Some(frame) => frame,
            None => reader
                .next_incoming()
                .await
                .map_err(|_| AppError::new(ErrorCode::Network, "宿主 provider IPC 已断开"))?,
        };
        match next {
            Incoming::Request { id, method, params } => {
                let outcome = runtime.handle(&method, params).await;
                writer.respond(&id, outcome).await.map_err(|_| {
                    AppError::new(ErrorCode::Network, "无法写入宿主 provider 应答")
                })?;
            }
            Incoming::Notification(notification) => {
                if let Some(Ok(event)) = notification.proxy_preference_changed() {
                    if event_requires_secret_fetch(&event) {
                        fetch_proxy_with_secret_on_stream(
                            &mut reader,
                            &mut writer,
                            &mut deferred,
                            &runtime,
                        )
                        .await?;
                    } else {
                        let (cfg, sys) = convert_proxy_event(&event);
                        runtime.update_global_proxy(cfg, sys);
                    }
                }
            }
            Incoming::Response { .. } => {
                return Err(AppError::new(
                    ErrorCode::InvalidResponse,
                    "宿主 provider 收到未匹配的响应",
                ));
            }
        }
    }
}

async fn fetch_proxy_with_secret_on_stream(
    reader: &mut StreamReader,
    writer: &mut StreamWriter,
    deferred: &mut VecDeque<Incoming>,
    runtime: &ProviderRuntime,
) -> AppResult<()> {
    let expected_id = writer
        .send_request("getProxyConfig", json!({ "include_secret": true }))
        .await
        .map_err(|_| AppError::new(ErrorCode::Network, "无法发送 getProxyConfig 请求"))?;
    let deadline = Instant::now() + PROXY_IPC_FETCH_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        let frame = match tokio::time::timeout(remaining, reader.next_incoming()).await {
            Ok(Ok(frame)) => frame,
            Ok(Err(_)) => {
                return Err(AppError::new(ErrorCode::Network, "宿主 provider IPC 已断开"));
            }
            Err(_) => return Ok(()),
        };
        match frame {
            Incoming::Response { id, result } if id == expected_id => {
                if let Ok(value) = result {
                    if let Ok(parsed) = serde_json::from_value::<ProxyConfigResult>(value) {
                        let (cfg, sys) = convert_client_proxy_result(&parsed);
                        runtime.update_global_proxy(cfg, sys);
                    }
                }
                return Ok(());
            }
            Incoming::Response { .. } => {
                return Err(AppError::new(
                    ErrorCode::InvalidResponse,
                    "宿主 provider 收到未匹配的响应",
                ));
            }
            other => {
                if deferred.len() < MAX_DEFERRED_FRAMES {
                    deferred.push_back(other);
                }
            }
        }
    }
}

fn parse_params<T: DeserializeOwned>(params: Value) -> Result<T, JsonRpcError> {
    if contains_reserved_credential_field(&params) {
        return Err(invalid_params());
    }
    serde_json::from_value(params).map_err(|_| invalid_params())
}

fn contains_reserved_credential_field(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_reserved_credential_field),
        Value::Object(fields) => fields.iter().any(|(name, value)| {
            is_reserved_credential_field(name) || contains_reserved_credential_field(value)
        }),
        _ => false,
    }
}

fn is_reserved_credential_field(name: &str) -> bool {
    let normalized: String = name
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect();
    matches!(
        normalized.as_str(),
        "apikey"
            | "apitoken"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "authorization"
            | "auth"
            | "secret"
            | "clientsecret"
            | "password"
            | "passwd"
            | "credential"
            | "credentials"
            | "bearer"
    ) || normalized.ends_with("apikey")
        || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || normalized.ends_with("password")
}

fn parse_cursor(value: &str) -> Result<u64, JsonRpcError> {
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_params());
    }
    value.parse().map_err(|_| invalid_params())
}

fn invalid_params() -> JsonRpcError {
    JsonRpcError {
        code: INVALID_PARAMS,
        message: "provider 参数无效".into(),
        data: None,
    }
}

fn provider_error(error: AppError) -> JsonRpcError {
    let code = match error.code() {
        ErrorCode::DeliveryUnknown => DELIVERY_UNKNOWN,
        ErrorCode::RateLimited => RATE_LIMITED,
        _ => PROVIDER_ERROR,
    };
    JsonRpcError {
        code,
        message: format!("{}: {}", error.stable_code(), error.message()),
        data: Some(json!({ "kind": error.stable_code() })),
    }
}

type ProbeParams = TransportProbeRequest;
type ReceiveParams = TransportReceiveRequest;
type SendParams = TransportSendRequest;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[cfg(unix)]
    use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, BufReader};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    fn fake_secret() -> crate::secret::SecretBuffer {
        let mut key = crate::secret::api_key_prefix().into_bytes();
        key.extend(std::iter::repeat_n(b'b', 64));
        crate::secret::SecretBuffer::from_bytes(key).unwrap()
    }

    async fn mailbox_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 1024];
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(request.starts_with(b"GET /openapi/v1/mailboxes "));
            let body = br#"{"data":[{"id":41,"address":"box@seek.li"}]}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(body).await.unwrap();
        });
        (format!("http://{address}/openapi/v1"), task)
    }

    async fn scripted_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0_u8; 1024];
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                captured
                    .lock()
                    .unwrap()
                    .push(request.lines().next().unwrap().to_owned());
                let reason = if status == 200 { "OK" } else { "Not Found" };
                let head = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).await.unwrap();
                stream.write_all(body.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}/openapi/v1"), requests, task)
    }

    async fn responses_then_stall_server(
        responses: Vec<&'static str>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            for body in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0_u8; 1024];
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).await.unwrap();
                stream.write_all(body.as_bytes()).await.unwrap();
            }
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 1024];
                let count = stream.read(&mut chunk).await.unwrap();
                if count == 0 {
                    return;
                }
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            std::future::pending::<()>().await;
        });
        (format!("http://{address}/openapi/v1"), task)
    }

    #[cfg(unix)]
    async fn read_json_line(reader: &mut (impl AsyncBufRead + Unpin)) -> Value {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        serde_json::from_str(&line).unwrap()
    }

    #[cfg(unix)]
    async fn write_json_line(writer: &mut (impl AsyncWrite + Unpin), value: &Value) {
        writer
            .write_all(value.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
    }

    #[test]
    fn cursor_is_strict_decimal() {
        assert_eq!(parse_cursor("0").unwrap(), 0);
        assert!(parse_cursor("").is_err());
        assert!(parse_cursor("+1").is_err());
        assert!(parse_cursor("18446744073709551616").is_err());
    }

    #[test]
    fn params_accept_additive_unknown_fields() {
        let probe = json!({
            "account": "a",
            "address": "box@seek.li",
            "direction": "incoming",
            "trace_context": {"request_id": "future-field"}
        });
        assert!(parse_params::<ProbeParams>(probe).is_ok());
        let receive = json!({
            "account": "a",
            "address": "box@seek.li",
            "since_uid": "0",
            "max": 1,
            "future_limit_hint": 10
        });
        assert!(parse_params::<ReceiveParams>(receive).is_ok());
        let send = json!({
            "account": "a",
            "account_address": "box@seek.li",
            "from": "box@seek.li",
            "to": "recipient@example.test",
            "future_delivery_options": ["plain-text"]
        });
        assert!(parse_params::<SendParams>(send).is_ok());
    }

    #[test]
    fn params_reject_reserved_credential_fields_at_any_depth() {
        for field in [
            "api_key",
            "apiKey",
            "api-token",
            "token",
            "Authorization",
            "client_secret",
            "password",
            "credentials",
        ] {
            let mut value = json!({
                "account": "a",
                "address": "box@seek.li",
                "direction": "incoming"
            });
            value
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), json!("must-not-be-accepted"));
            assert!(parse_params::<ProbeParams>(value).is_err(), "{field}");
        }
        let nested = json!({
            "account": "a",
            "address": "box@seek.li",
            "direction": "incoming",
            "future": {"access_token": "must-not-be-accepted"}
        });
        assert!(parse_params::<ProbeParams>(nested).is_err());
    }

    #[test]
    fn provider_error_is_stable_and_bounded() {
        let error = provider_error(AppError::new(
            ErrorCode::DeliveryUnknown,
            "邮件投递结果未知；已停止自动重放",
        ));
        assert_eq!(error.code, DELIVERY_UNKNOWN);
        assert!(!error.message.contains('@'));
        assert!(error.message.len() < 128);
    }

    #[test]
    fn alias_from_is_rejected_before_network_or_state_access() {
        assert!(ensure_primary_from("alias@seek.li", "box@seek.li").is_err());
        assert!(ensure_primary_from("box@seek.li", "box@seek.li").is_ok());
    }

    #[test]
    fn transient_detail_failure_returns_only_contiguous_partial_progress() {
        assert!(should_return_partial(
            &AppError::new(ErrorCode::RateLimited, "limited"),
            1
        ));
        assert!(should_return_partial(
            &AppError::new(ErrorCode::Network, "network"),
            1
        ));
        assert!(!should_return_partial(
            &AppError::new(ErrorCode::NotFound, "missing"),
            1
        ));
        assert!(!should_return_partial(
            &AppError::new(ErrorCode::Timeout, "timeout"),
            0
        ));
    }

    #[tokio::test]
    async fn missing_binding_is_imported_from_exact_remote_address() {
        let (origin, server) = mailbox_server().await;
        let root = tempfile::tempdir().unwrap();
        let state = StateStore::at_root(root.path().join("state")).unwrap();
        let runtime = ProviderRuntime::for_test(
            ApiClient::for_test(fake_secret(), &origin).unwrap(),
            state.clone(),
            None,
        );
        let binding = runtime
            .checked_binding("account-a", "box@seek.li")
            .await
            .unwrap();
        assert_eq!(binding.mailbox_id, 41);
        assert_eq!(state.binding("account-a").unwrap(), binding);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn draft_probe_rejects_stale_owner_before_network_then_allows_released_address() {
        let (origin, server) = mailbox_server().await;
        let root = tempfile::tempdir().unwrap();
        let state = StateStore::at_root(root.path().join("state")).unwrap();
        state
            .import_mailbox("account-old", "box@seek.li", 41)
            .unwrap();
        let runtime = ProviderRuntime::for_test(
            ApiClient::for_test(fake_secret(), &origin).unwrap(),
            state.clone(),
            None,
        );
        let params = json!({
            "account":"",
            "address":"box@SEEK.LI",
            "direction":"incoming"
        });

        let error = runtime
            .handle("transport/probe", params.clone())
            .await
            .unwrap_err();
        assert_eq!(error.data.unwrap()["kind"], "conflict");

        state
            .unimport_mailbox("account-old", "box@seek.li")
            .unwrap();
        let result = runtime.handle("transport/probe", params).await.unwrap();
        assert_eq!(result["ok"], true);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn existing_account_probe_does_not_import_an_unbound_mailbox() {
        let (origin,server)=mailbox_server().await;
        let root=tempfile::tempdir().unwrap();
        let state=StateStore::at_root(root.path().join("state")).unwrap();
        let runtime=ProviderRuntime::for_test(ApiClient::for_test(fake_secret(),&origin).unwrap(),state.clone(),None);
        let result=runtime.handle("transport/probe",json!({"account":"account-existing","address":"box@seek.li","direction":"incoming"})).await;
        assert_eq!(state.binding("account-existing").unwrap_err().code(),ErrorCode::NotFound,"probe must not import a binding before the user saves");
        assert_eq!(result.unwrap()["ok"],true);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn receive_advances_one_persisted_discovery_page_then_returns_details() {
        let list = r#"{
            "data":[
                {"id":1,"mailboxId":41},{"id":2,"mailboxId":41},
                {"id":3,"mailboxId":41},{"id":4,"mailboxId":41},
                {"id":5,"mailboxId":41},{"id":6,"mailboxId":41},
                {"id":7,"mailboxId":41},{"id":8,"mailboxId":41},
                {"id":9,"mailboxId":41},{"id":10,"mailboxId":41}
            ],
            "pagination":{"total":20,"limit":10,"offset":0}
        }"#;
        let detail = r#"{"data":{"id":1,"mailboxId":41,"fromAddress":"sender@example.test","toAddress":"box@seek.li","subject":"one","bodyText":"body","receivedAt":"2026-08-23T00:00:00Z"}}"#;
        let (origin, requests, server) = scripted_server(vec![(200, list), (200, detail)]).await;
        let root = tempfile::tempdir().unwrap();
        let state = StateStore::at_root(root.path().join("state")).unwrap();
        state
            .import_mailbox("account-a", "box@seek.li", 41)
            .unwrap();
        let runtime = ProviderRuntime::for_test(
            ApiClient::for_test(fake_secret(), &origin).unwrap(),
            state.clone(),
            None,
        );
        let result = runtime
            .handle(
                "transport/receive",
                json!({
                    "account":"account-a",
                    "address":"box@seek.li",
                    "since_uid":"0",
                    "max":1
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["messages"][0]["uid"], "1");
        assert_eq!(state.scan_offset("account-a").unwrap(), 10);
        assert_eq!(state.pending("account-a", 1, 20).unwrap().len(), 9);
        server.await.unwrap();
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains("/mailboxes/41/mails?"));
        assert!(requests[1].contains("/mails/1 "));
    }

    #[tokio::test]
    async fn deleted_remote_mail_is_tombstoned_without_blocking_later_uid() {
        let missing = r#"{"error":{"code":"not_found","message":"gone"}}"#;
        let detail = r#"{"data":{"id":20,"mailboxId":41,"fromAddress":"sender@example.test","toAddress":"box@seek.li","subject":"later","bodyText":"body","receivedAt":"2026-08-23T00:00:00Z"}}"#;
        let (origin, _requests, server) =
            scripted_server(vec![(404, missing), (200, detail)]).await;
        let root = tempfile::tempdir().unwrap();
        let state = StateStore::at_root(root.path().join("state")).unwrap();
        state
            .import_mailbox("account-a", "box@seek.li", 41)
            .unwrap();
        state.assign_remote_ids("account-a", &[10, 20]).unwrap();
        let runtime = ProviderRuntime::for_test(
            ApiClient::for_test(fake_secret(), &origin).unwrap(),
            state.clone(),
            None,
        );
        let result = runtime
            .handle(
                "transport/receive",
                json!({
                    "account":"account-a",
                    "address":"box@seek.li",
                    "since_uid":"0",
                    "max":20
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["messages"].as_array().unwrap().len(), 1);
        assert_eq!(result["messages"][0]["uid"], "2");
        assert_eq!(state.pending("account-a", 0, 20).unwrap().len(), 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn receive_returns_contiguous_partial_progress_before_operation_budget() {
        let detail = r#"{"data":{"id":10,"mailboxId":41,"fromAddress":"sender@example.test","toAddress":"box@seek.li","subject":"first","bodyText":"body","receivedAt":"2026-08-23T00:00:00Z"}}"#;
        let (origin, server) = responses_then_stall_server(vec![detail]).await;
        let root = tempfile::tempdir().unwrap();
        let state = StateStore::at_root(root.path().join("state")).unwrap();
        state
            .import_mailbox("account-a", "box@seek.li", 41)
            .unwrap();
        state.assign_remote_ids("account-a", &[10, 20]).unwrap();
        let runtime = ProviderRuntime::for_test(
            ApiClient::for_test_with_read_timeout(
                fake_secret(),
                &origin,
                Duration::from_secs(2),
            )
            .unwrap(),
            state,
            None,
        );
        let started = Instant::now();
        let result = runtime
            .receive_with_budget(
                ReceiveParams {
                    account: "account-a".into(),
                    address: "box@seek.li".into(),
                    since_uid: "0".into(),
                    max: 20,
                },
                Duration::from_millis(350),
                Duration::from_millis(100),
            )
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["uid"], "1");
        server.abort();
    }

    #[tokio::test]
    async fn receive_without_progress_fails_at_bounded_deadline() {
        let (origin, server) = responses_then_stall_server(Vec::new()).await;
        let root = tempfile::tempdir().unwrap();
        let state = StateStore::at_root(root.path().join("state")).unwrap();
        state
            .import_mailbox("account-a", "box@seek.li", 41)
            .unwrap();
        state.assign_remote_ids("account-a", &[10]).unwrap();
        let runtime = ProviderRuntime::for_test(
            ApiClient::for_test_with_read_timeout(
                fake_secret(),
                &origin,
                Duration::from_secs(2),
            )
            .unwrap(),
            state,
            None,
        );
        let started = Instant::now();
        let error = runtime
            .receive_with_budget(
                ReceiveParams {
                    account: "account-a".into(),
                    address: "box@seek.li".into(),
                    since_uid: "0".into(),
                    max: 20,
                },
                Duration::from_millis(300),
                Duration::from_millis(100),
            )
            .await
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(error.data.unwrap()["kind"], "timeout");
        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn provider_ipc_authenticates_registers_and_serves_basic_transport_chain() {
        let mailbox = r#"{"data":{"id":41,"address":"box@seek.li","sendDisabled":false}}"#;
        let detail = r#"{"data":{"id":10,"mailboxId":41,"fromAddress":"sender@example.test","toAddress":"box@seek.li","subject":"inbound","bodyText":"body","receivedAt":"2026-08-23T00:00:00Z"}}"#;
        let (origin, http_requests, http_server) = scripted_server(vec![
            (200, mailbox),
            (200, detail),
            (200, mailbox),
            (200, ""),
        ])
        .await;
        let root = tempfile::tempdir().unwrap();
        let state = StateStore::at_root(root.path().join("state")).unwrap();
        state
            .import_mailbox("account-a", "box@seek.li", 41)
            .unwrap();
        state.assign_remote_ids("account-a", &[10]).unwrap();
        let runtime = ProviderRuntime::for_test(
            ApiClient::for_test(fake_secret(), &origin).unwrap(),
            state,
            None,
        );

        let socket = root.path().join(maild_brand::SOCKET_FILE_NAME);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let ipc_server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = tokio::io::split(stream);
            let mut read = BufReader::new(read);

            let auth = read_json_line(&mut read).await;
            assert_eq!(auth["method"], "authenticate");
            assert_eq!(auth["params"]["token"], "mdp_test_token");
            write_json_line(
                &mut write,
                &json!({"jsonrpc":"2.0","id":auth["id"],"result":{}}),
            )
            .await;

            let register = read_json_line(&mut read).await;
            assert_eq!(register["method"], "providerRegister");
            write_json_line(
                &mut write,
                &json!({"jsonrpc":"2.0","id":register["id"],"result":{}}),
            )
            .await;

            let get_proxy = read_json_line(&mut read).await;
            assert_eq!(get_proxy["method"], "getProxyConfig");
            assert_eq!(get_proxy["params"]["include_secret"], true);
            write_json_line(
                &mut write,
                &json!({
                    "jsonrpc":"2.0",
                    "id":get_proxy["id"],
                    "result":{
                        "config":{"mode":"direct"},
                        "has_password":false,
                        "effective_system":{"bypass":[],"exclude_simple_hostnames":false,"source":"none"},
                        "persisted":true
                    }
                }),
            )
            .await;

            write_json_line(
                &mut write,
                &json!({
                    "jsonrpc":"2.0","id":"probe-1","method":"transport/probe",
                    "params":{"account":"account-a","address":"box@seek.li","direction":"incoming"}
                }),
            )
            .await;
            let probe = read_json_line(&mut read).await;

            write_json_line(
                &mut write,
                &json!({
                    "jsonrpc":"2.0","id":"receive-1","method":"transport/receive",
                    "params":{"account":"account-a","address":"box@seek.li","since_uid":"0","max":20}
                }),
            )
            .await;
            let receive = read_json_line(&mut read).await;

            write_json_line(
                &mut write,
                &json!({
                    "jsonrpc":"2.0","id":"send-1","method":"transport/send",
                    "params":{
                        "account":"account-a","account_address":"box@seek.li",
                        "from":"box@seek.li","to":"recipient@example.test",
                        "subject":"outbound","body":"body"
                    }
                }),
            )
            .await;
            let send = read_json_line(&mut read).await;
            (probe, receive, send)
        });

        let provider = tokio::spawn(run_with_runtime(
            runtime,
            Endpoint::Unix {
                path: socket.clone(),
            },
            "mdp_test_token",
        ));
        let (probe, receive, send) = ipc_server.await.unwrap();
        assert_eq!(probe["id"], "probe-1");
        assert_eq!(probe["result"]["ok"], true);
        assert_eq!(receive["id"], "receive-1");
        assert_eq!(receive["result"]["messages"][0]["uid"], "1");
        assert_eq!(send["id"], "send-1");
        assert_eq!(send["result"]["ok"], true);
        assert_eq!(
            provider.await.unwrap().unwrap_err().code(),
            ErrorCode::Network
        );
        http_server.await.unwrap();
        let requests = http_requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[3].starts_with("POST /openapi/v1/send "));
    }
}
