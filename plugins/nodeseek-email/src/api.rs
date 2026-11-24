use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use maild_proxy::{ProxyConfig, ProxyOverride, SystemProxySnapshot};
use reqwest::header::{HeaderMap, CONTENT_LENGTH, RETRY_AFTER};
use reqwest::{Method, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::{AppError, AppResult, ErrorCode};
use crate::proxy::ProxySnapshot;
use crate::secret::SecretBuffer;
use crate::state::canonicalize_address;
use crate::PRODUCTION_ORIGIN;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
// Host provider probes have a 10-second per-call budget. Keep one complete read operation,
// including its single retry, inside that boundary so a late stale response cannot block the
// provider connection behind it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const MAIL_DISCOVERY_PAGE_SIZE: usize = 10;
const MAX_MAIL_OFFSET: usize = 1_000_000;
const MAX_RATE_DELAY_SECONDS: u64 = 300;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mailbox {
    pub id: i64,
    pub address: String,
    pub status: Option<i64>,
    pub send_disabled: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteMail {
    pub id: i64,
    pub mailbox_id: Option<i64>,
    pub sender: String,
    pub recipient: String,
    pub subject: String,
    pub body_text: String,
    pub received_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Quota {
    /// Compatibility view for the live API's observed top-level `current` field. The OpenAPI
    /// snapshot does not define its shape, so it is retained only after conservative validation.
    pub current: Option<Value>,
    pub storage_used: Option<i64>,
    pub storage_quota: Option<i64>,
    pub storage_usage_percent: Option<i64>,
    pub storage_quota_source: Option<String>,
    pub mailbox_count: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateOutcome {
    pub mailbox: Mailbox,
    pub reconciled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteMailPage {
    pub ids: Vec<i64>,
    /// Offset to persist for the next discovery pass. Zero means wrap to the first page.
    pub next_offset: usize,
}

#[derive(Default)]
struct RateGate {
    not_before_unix: AtomicU64,
}

impl RateGate {
    fn check(&self) -> AppResult<()> {
        if unix_now() < self.not_before_unix.load(Ordering::Acquire) {
            return Err(AppError::new(
                ErrorCode::RateLimited,
                "NodeSeek Email 请求受共享限流闸门阻止",
            ));
        }
        Ok(())
    }

    fn observe(&self, status: StatusCode, headers: &HeaderMap) {
        let now = unix_now();
        let exhausted = status == StatusCode::TOO_MANY_REQUESTS
            || header_u64(headers, "x-ratelimit-remaining") == Some(0);
        if !exhausted {
            return;
        }
        let reset = retry_after(headers, now)
            .or_else(|| {
                header_u64(headers, "x-ratelimit-reset").map(|value| {
                    if value > now {
                        value
                    } else if value <= 86_400 {
                        now.saturating_add(value)
                    } else {
                        now.saturating_add(1)
                    }
                })
            })
            .unwrap_or_else(|| now.saturating_add(60));
        let bounded = reset.clamp(
            now.saturating_add(1),
            now.saturating_add(MAX_RATE_DELAY_SECONDS),
        );
        self.not_before_unix.fetch_max(bounded, Ordering::AcqRel);
    }
}

pub struct ApiClient {
    http: Arc<RwLock<reqwest::Client>>,
    proxy: Arc<RwLock<ProxySnapshot>>,
    origin: String,
    secret: Arc<SecretBuffer>,
    gate: Arc<RateGate>,
    mailbox_cache: Arc<Mutex<HashMap<String, i64>>>,
    read_timeout: Duration,
}

impl ApiClient {
    pub fn production(secret: SecretBuffer) -> AppResult<Self> {
        Self::production_with_proxy(secret, ProxySnapshot::default_system())
    }

    pub fn production_with_proxy(secret: SecretBuffer, proxy: ProxySnapshot) -> AppResult<Self> {
        Self::build(secret, PRODUCTION_ORIGIN, false, proxy)
    }

    #[cfg(test)]
    pub(crate) fn for_test(secret: SecretBuffer, origin: &str) -> AppResult<Self> {
        Self::build(
            secret,
            origin,
            true,
            ProxySnapshot::from_global(
                ProxyConfig {
                    mode: maild_proxy::ProxyMode::Direct,
                    custom: None,
                },
                SystemProxySnapshot::default(),
            ),
        )
    }

    #[cfg(any(test, debug_assertions))]
    pub fn for_test_with_proxy(
        secret: SecretBuffer,
        origin: &str,
        proxy: ProxySnapshot,
    ) -> AppResult<Self> {
        Self::build(secret, origin, true, proxy)
    }

    fn build(
        secret: SecretBuffer,
        origin: &str,
        allow_test_origin: bool,
        proxy: ProxySnapshot,
    ) -> AppResult<Self> {
        validate_origin(origin, allow_test_origin)?;
        let http = build_reqwest_client(&proxy)?;
        Ok(Self {
            http: Arc::new(RwLock::new(http)),
            proxy: Arc::new(RwLock::new(proxy)),
            origin: origin.trim_end_matches('/').to_owned(),
            secret: Arc::new(secret),
            gate: Arc::new(RateGate::default()),
            mailbox_cache: Arc::new(Mutex::new(HashMap::new())),
            read_timeout: REQUEST_TIMEOUT,
        })
    }

    /// Atomically replace the active [`ProxySnapshot`] and rebuild the underlying `reqwest::Client`
    /// so stale pooled connections to a previous proxy are dropped immediately.
    pub fn update_proxy(&self, next: ProxySnapshot) -> AppResult<()> {
        let client = build_reqwest_client(&next)?;
        {
            let mut proxy_guard = self
                .proxy
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut http_guard = self
                .http
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *proxy_guard = next;
            *http_guard = client;
        }
        Ok(())
    }

    /// Hot-reload the inherited global proxy config while preserving this plugin's local override
    /// and any extra programmatic resolver.
    pub fn update_global_proxy(
        &self,
        global_config: ProxyConfig,
        global_system: SystemProxySnapshot,
    ) -> AppResult<()> {
        let mut next = self
            .proxy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if next.global_config == global_config && next.global_system == global_system {
            return Ok(());
        }
        next.global_config = global_config;
        next.global_system = global_system;
        self.update_proxy(next)
    }

    /// Hot-reload this plugin's local [`ProxyOverride`] while preserving the inherited global config.
    pub fn update_plugin_override(&self, plugin_override: ProxyOverride) -> AppResult<()> {
        let mut next = self
            .proxy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if next.plugin_override == plugin_override {
            return Ok(());
        }
        next.plugin_override = plugin_override;
        self.update_proxy(next)
    }

    /// Snapshot of current proxy settings (redacted for diagnostics/tests; never leaks secrets).
    pub fn proxy_override(&self) -> ProxyOverride {
        self.proxy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .plugin_override
            .without_secret()
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_read_timeout(
        secret: SecretBuffer,
        origin: &str,
        read_timeout: Duration,
    ) -> AppResult<Self> {
        if read_timeout.is_zero() {
            return Err(AppError::invalid_input());
        }
        let mut client = Self::for_test(secret, origin)?;
        client.read_timeout = read_timeout;
        Ok(client)
    }

    pub async fn list_mailboxes(&self) -> AppResult<Vec<Mailbox>> {
        let envelope: Envelope<Vec<MailboxWire>> = self.get_json("mailboxes", &[]).await?;
        let rows = envelope.data.ok_or_else(AppError::invalid_response)?;
        let mut mailboxes = Vec::with_capacity(rows.len());
        for row in rows {
            let mailbox = Mailbox::try_from(row)?;
            self.cache(&mailbox);
            mailboxes.push(mailbox);
        }
        Ok(mailboxes)
    }

    pub async fn mailbox(&self, mailbox_id: i64) -> AppResult<Mailbox> {
        if mailbox_id <= 0 {
            return Err(AppError::invalid_input());
        }
        let path = format!("mailboxes/{mailbox_id}");
        let envelope: Envelope<MailboxWire> = self.get_json(&path, &[]).await?;
        let mailbox = Mailbox::try_from(envelope.data.ok_or_else(AppError::invalid_response)?)?;
        if mailbox.id != mailbox_id {
            return Err(AppError::invalid_response());
        }
        self.cache(&mailbox);
        Ok(mailbox)
    }

    pub async fn resolve_mailbox(&self, address: &str) -> AppResult<Mailbox> {
        let address = canonicalize_address(address)?;
        let cached = self
            .mailbox_cache
            .lock()
            .map_err(|_| AppError::state())?
            .get(&address)
            .copied();
        if let Some(id) = cached {
            let mailbox = self.mailbox(id).await?;
            if mailbox.address != address {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "远端邮箱地址与本地缓存不一致",
                ));
            }
            return Ok(mailbox);
        }
        self.list_mailboxes()
            .await?
            .into_iter()
            .find(|mailbox| mailbox.address == address)
            .ok_or_else(|| AppError::new(ErrorCode::NotFound, "NodeSeek Email 中不存在该邮箱"))
    }

    pub async fn check_mailbox(&self, address: &str) -> AppResult<bool> {
        let address = canonicalize_address(address)?;
        let envelope: Envelope<AvailabilityWire> = self
            .post_json(
                "mailboxes/check",
                &[("address", address)],
                None::<&()>,
                None,
            )
            .await?;
        envelope
            .data
            .and_then(|value| value.available)
            .ok_or_else(AppError::invalid_response)
    }

    pub async fn create_mailbox(&self, address: &str) -> AppResult<CreateOutcome> {
        let address = canonicalize_address(address)?;
        let body = serde_json::json!({ "address": address });
        let result: AppResult<Mailbox> = self
            .post_json("mailboxes", &[], Some(&body), Some(StatusCode::CREATED))
            .await
            .and_then(|envelope: Envelope<MailboxWire>| {
                Mailbox::try_from(envelope.data.ok_or_else(AppError::invalid_response)?)
            })
            .and_then(|mailbox| {
                if mailbox.address == address {
                    Ok(mailbox)
                } else {
                    Err(AppError::invalid_response())
                }
            });
        match result {
            Ok(mailbox) => {
                self.cache(&mailbox);
                Ok(CreateOutcome {
                    mailbox,
                    reconciled: false,
                })
            }
            Err(error) if error.possibly_applied() => {
                let reconciled = self.list_mailboxes().await.ok().and_then(|mailboxes| {
                    mailboxes
                        .into_iter()
                        .find(|mailbox| mailbox.address == address)
                });
                match reconciled {
                    Some(mailbox) => Ok(CreateOutcome {
                        mailbox,
                        reconciled: true,
                    }),
                    None => Err(AppError::new(
                        ErrorCode::CreateUnknown,
                        "邮箱创建结果未知；已停止自动重放",
                    )),
                }
            }
            Err(error) => Err(error),
        }
    }

    pub async fn quota(&self) -> AppResult<Quota> {
        let envelope: Envelope<QuotaWire> = self.get_json("quotas", &[]).await?;
        let value = envelope.data.ok_or_else(AppError::invalid_response)?;
        let quota_source = value
            .storage
            .as_ref()
            .and_then(|storage| storage.quota_source.as_deref());
        if quota_source
            .is_some_and(|source| source.len() > 128 || source.chars().any(char::is_control))
        {
            return Err(AppError::invalid_response());
        }
        let numeric_fields = [
            value.storage.as_ref().and_then(|storage| storage.used),
            value.storage.as_ref().and_then(|storage| storage.quota),
            value
                .storage
                .as_ref()
                .and_then(|storage| storage.usage_percent),
            value
                .mailboxes
                .as_ref()
                .and_then(|mailboxes| mailboxes.count),
        ];
        if numeric_fields.into_iter().flatten().any(|value| value < 0) {
            return Err(AppError::invalid_response());
        }
        if let Some(current) = value.current.as_ref() {
            validate_quota_current(current)?;
        }
        Ok(Quota {
            current: value.current,
            storage_used: value.storage.as_ref().and_then(|storage| storage.used),
            storage_quota: value.storage.as_ref().and_then(|storage| storage.quota),
            storage_usage_percent: value
                .storage
                .as_ref()
                .and_then(|storage| storage.usage_percent),
            storage_quota_source: quota_source.map(ToOwned::to_owned),
            mailbox_count: value.mailboxes.and_then(|mailboxes| mailboxes.count),
        })
    }

    pub async fn remote_mail_ids_page(
        &self,
        mailbox_id: i64,
        offset: usize,
    ) -> AppResult<RemoteMailPage> {
        if mailbox_id <= 0 || offset > MAX_MAIL_OFFSET {
            return Err(AppError::invalid_input());
        }
        let path = format!("mailboxes/{mailbox_id}/mails");
        let query = [
            ("folder", "inbox".to_owned()),
            ("limit", MAIL_DISCOVERY_PAGE_SIZE.to_string()),
            ("offset", offset.to_string()),
        ];
        let page: MailPageWire = self.get_json(&path, &query).await?;
        let rows = page.data.ok_or_else(AppError::invalid_response)?;
        let pagination = page.pagination;
        let reported_limit = match pagination.as_ref().and_then(|pagination| pagination.limit) {
            Some(limit) => usize::try_from(limit).map_err(|_| AppError::invalid_response())?,
            None => MAIL_DISCOVERY_PAGE_SIZE,
        };
        if reported_limit == 0
            || reported_limit > MAIL_DISCOVERY_PAGE_SIZE
            || rows.len() > reported_limit
            || pagination
                .as_ref()
                .and_then(|pagination| pagination.offset)
                .is_some_and(|reported| usize::try_from(reported).ok() != Some(offset))
        {
            return Err(AppError::invalid_response());
        }
        let row_count = rows.len();
        let mut ids = BTreeSet::new();
        for row in rows {
            // Some live responses currently ignore the path mailbox id and return a page from
            // the user's other mailboxes. Never enqueue those ids, and never accept a row whose
            // ownership is omitted: detail requests must remain scoped to the imported mailbox.
            if row.mailbox_id != Some(mailbox_id) {
                continue;
            }
            let id = require_positive(row.id)?;
            ids.insert(id);
        }
        let candidate = offset
            .checked_add(row_count)
            .filter(|next| *next <= MAX_MAIL_OFFSET)
            .ok_or_else(AppError::invalid_response)?;
        let reported_total = pagination.and_then(|pagination| pagination.total);
        if reported_total.is_some_and(|total| total < 0) {
            return Err(AppError::invalid_response());
        }
        let finished = row_count < reported_limit
            || reported_total.is_some_and(|total| candidate as i64 >= total);
        Ok(RemoteMailPage {
            ids: ids.into_iter().collect(),
            next_offset: if finished { 0 } else { candidate },
        })
    }

    pub async fn mail(&self, mailbox_id: i64, remote_mail_id: i64) -> AppResult<RemoteMail> {
        if mailbox_id <= 0 || remote_mail_id <= 0 {
            return Err(AppError::invalid_input());
        }
        let path = format!("mails/{remote_mail_id}");
        let envelope: Envelope<Value> = self.get_json(&path, &[]).await?;
        let data = envelope.data.ok_or_else(AppError::invalid_response)?;
        // The live API wraps detail responses as data.mail, while the OpenAPI snapshot and
        // earlier deployments returned the mail object directly under data. Accept both shapes,
        // but keep the existing strict field and mailbox ownership checks below.
        let row_value = data.get("mail").cloned().unwrap_or(data);
        let row: MailWire =
            serde_json::from_value(row_value).map_err(|_| AppError::invalid_response())?;
        let id = require_positive(row.id)?;
        if id != remote_mail_id || row.mailbox_id.is_some_and(|value| value != mailbox_id) {
            return Err(AppError::invalid_response());
        }
        let sender = require_text(row.from_address)?;
        let recipient = require_text(row.to_address)?;
        maild_client::validate_send_field(maild_client::SendField::From, &sender)
            .and_then(|_| {
                maild_client::validate_send_field(maild_client::SendField::To, &recipient)
            })
            .map_err(|_| AppError::invalid_response())?;
        let received_at = parse_received_at(row.received_at)?;
        let subject = row.subject.unwrap_or_default();
        let body_text = row.body_text.unwrap_or_default();
        if subject.len() > maild_client::MAX_SUBJECT_BYTES
            || body_text.len() > maild_client::MAX_BODY_BYTES
            || subject.chars().any(char::is_control)
            || body_text
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\r' | '\n' | '\t'))
        {
            return Err(AppError::invalid_response());
        }
        Ok(RemoteMail {
            id,
            mailbox_id: row.mailbox_id,
            sender,
            recipient,
            subject,
            body_text,
            received_at,
        })
    }

    pub async fn send_plain_text(
        &self,
        mailbox_id: i64,
        to: &str,
        subject: &str,
        body_text: &str,
    ) -> AppResult<()> {
        if mailbox_id <= 0 {
            return Err(AppError::invalid_input());
        }
        validate_outgoing(to, subject, body_text)?;
        let body = SendWire {
            mailbox_id,
            to: vec![to],
            subject,
            body_text,
        };
        let result = self.post_unit("send", &[], &body).await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if error.possibly_applied() => Err(AppError::new(
                ErrorCode::DeliveryUnknown,
                "邮件投递结果未知；已停止自动重放",
            )),
            Err(error) => Err(error),
        }
    }

    fn cache(&self, mailbox: &Mailbox) {
        if let Ok(mut cache) = self.mailbox_cache.lock() {
            cache.insert(mailbox.address.clone(), mailbox.id);
        }
    }

    async fn get_json<T>(&self, path: &str, query: &[(&str, String)]) -> AppResult<T>
    where
        T: DeserializeOwned,
    {
        let deadline = tokio::time::Instant::now() + self.read_timeout;
        let first = tokio::time::timeout_at(
            deadline,
            self.request_json(Method::GET, path, query, None::<&()>),
        )
        .await
        .unwrap_or_else(|_| Err(AppError::new(ErrorCode::Timeout, "NodeSeek Email 请求超时")));
        match first {
            Err(error)
                if matches!(
                    error.code(),
                    ErrorCode::Network | ErrorCode::Timeout | ErrorCode::Upstream
                ) =>
            {
                if tokio::time::Instant::now() + Duration::from_millis(50) >= deadline {
                    return Err(error);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                tokio::time::timeout_at(
                    deadline,
                    self.request_json(Method::GET, path, query, None::<&()>),
                )
                .await
                .unwrap_or_else(|_| {
                    Err(AppError::new(ErrorCode::Timeout, "NodeSeek Email 请求超时"))
                })
            }
            other => other,
        }
    }

    async fn post_json<T, B>(
        &self,
        path: &str,
        query: &[(&str, String)],
        body: Option<&B>,
        expected: Option<StatusCode>,
    ) -> AppResult<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let response = self
            .request_bytes(Method::POST, path, query, body, expected)
            .await?;
        serde_json::from_slice(&response).map_err(|_| AppError::invalid_response())
    }

    async fn post_unit<B>(&self, path: &str, query: &[(&str, String)], body: &B) -> AppResult<()>
    where
        B: Serialize + ?Sized,
    {
        self.request_bytes(Method::POST, path, query, Some(body), None)
            .await
            .map(|_| ())
    }

    async fn request_json<T, B>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&B>,
    ) -> AppResult<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let response = self.request_bytes(method, path, query, body, None).await?;
        serde_json::from_slice(&response).map_err(|_| AppError::invalid_response())
    }

    async fn request_bytes<B>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&B>,
        expected: Option<StatusCode>,
    ) -> AppResult<Vec<u8>>
    where
        B: Serialize + ?Sized,
    {
        self.gate.check()?;
        let url = self.url(path, query)?;
        let key = self.secret.as_str()?;
        let client = self
            .http
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut request = client.request(method, url).bearer_auth(key);
        if let Some(body) = body {
            request = request.json(body);
        }
        let mut response = request.send().await.map_err(classify_transport_error)?;
        let status = response.status();
        self.gate.observe(status, response.headers());
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > MAX_RESPONSE_BYTES)
        {
            return Err(AppError::new(
                ErrorCode::ResponseTooLarge,
                "NodeSeek Email 响应超过安全上限",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(classify_transport_error)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(AppError::new(
                    ErrorCode::ResponseTooLarge,
                    "NodeSeek Email 响应超过安全上限",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if status == StatusCode::TOO_MANY_REQUESTS
            || expected.is_some_and(|expected| status != expected)
            || (expected.is_none() && !status.is_success())
        {
            return Err(classify_status(status, &bytes));
        }
        Ok(bytes)
    }

    fn url(&self, path: &str, query: &[(&str, String)]) -> AppResult<Url> {
        if path.is_empty() || path.starts_with('/') || path.contains(['?', '#']) {
            return Err(AppError::invalid_input());
        }
        let mut url = Url::parse(&format!("{}/{}", self.origin, path))
            .map_err(|_| AppError::invalid_input())?;
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
        Ok(url)
    }
}

fn build_reqwest_client(proxy: &ProxySnapshot) -> AppResult<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT);
    proxy
        .apply_reqwest_proxy(builder)
        .build()
        .map_err(|_| AppError::new(ErrorCode::Network, "无法初始化 NodeSeek Email HTTP 客户端"))
}

fn validate_origin(origin: &str, allow_test_origin: bool) -> AppResult<()> {
    if !allow_test_origin {
        if origin != PRODUCTION_ORIGIN {
            return Err(AppError::invalid_input());
        }
        return Ok(());
    }
    let parsed = Url::parse(origin).map_err(|_| AppError::invalid_input())?;
    let loopback = parsed
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|address| address.is_loopback());
    if parsed.scheme() != "http"
        || !loopback
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(AppError::invalid_input());
    }
    Ok(())
}

fn classify_transport_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        AppError::new(ErrorCode::Timeout, "NodeSeek Email 请求超时")
    } else {
        AppError::new(ErrorCode::Network, "NodeSeek Email 网络请求失败")
    }
}

fn classify_status(status: StatusCode, body: &[u8]) -> AppError {
    let (code, fallback) = match status {
        StatusCode::UNAUTHORIZED => (
            ErrorCode::Authentication,
            "NodeSeek Email API key 未通过认证",
        ),
        StatusCode::FORBIDDEN => (ErrorCode::Forbidden, "NodeSeek Email API key 权限不足"),
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            (ErrorCode::InvalidInput, "NodeSeek Email 拒绝了请求参数")
        }
        StatusCode::NOT_FOUND => (ErrorCode::NotFound, "NodeSeek Email 资源不存在"),
        StatusCode::CONFLICT => (ErrorCode::Conflict, "NodeSeek Email 资源冲突"),
        StatusCode::TOO_MANY_REQUESTS => {
            (ErrorCode::RateLimited, "NodeSeek Email 请求达到限流阈值")
        }
        status if status.is_server_error() => {
            (ErrorCode::Upstream, "NodeSeek Email 服务暂时不可用")
        }
        _ => (
            ErrorCode::InvalidResponse,
            "NodeSeek Email 返回了未预期的 HTTP 状态",
        ),
    };
    match upstream_error_summary(body) {
        Some(detail) => AppError::with_detail(code, fallback, detail),
        None => AppError::new(code, fallback),
    }
}

fn upstream_error_summary(body: &[u8]) -> Option<String> {
    let envelope: ErrorEnvelopeWire = serde_json::from_slice(body).ok()?;
    let error = envelope.error?;
    let code = sanitize_error_text(&error.code, 64)?;
    let message = sanitize_error_text(&error.message, 192)?;
    let prefix = crate::secret::api_key_prefix();
    if code.contains(&prefix) || message.contains(&prefix) {
        return Some("NodeSeek Email 上游错误内容已隐藏".into());
    }
    Some(format!("NodeSeek Email {code}: {message}"))
}

fn sanitize_error_text(value: &str, max_bytes: usize) -> Option<String> {
    let mut out = String::new();
    for character in value.chars().filter(|character| !character.is_control()) {
        if out.len() + character.len_utf8() > max_bytes {
            break;
        }
        out.push(character);
    }
    let trimmed = out.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn header_u64(headers: &HeaderMap, name: &'static str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn retry_after(headers: &HeaderMap, now: u64) -> Option<u64> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(now.saturating_add(seconds));
    }
    let date = httpdate::parse_http_date(value).ok()?;
    date.duration_since(UNIX_EPOCH)
        .ok()
        .map(|value| value.as_secs())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn require_positive(value: Option<i64>) -> AppResult<i64> {
    value
        .filter(|value| *value > 0)
        .ok_or_else(AppError::invalid_response)
}

fn require_text(value: Option<String>) -> AppResult<String> {
    value
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .ok_or_else(AppError::invalid_response)
}

const LATEST_PLAUSIBLE_DATE_SECS: u64 = 253_402_300_799;

fn parse_received_at(value: Option<Value>) -> AppResult<u64> {
    let value = value.ok_or_else(AppError::invalid_response)?;
    if let Some(timestamp) = value.as_u64() {
        return (timestamp <= LATEST_PLAUSIBLE_DATE_SECS)
            .then_some(timestamp)
            .ok_or_else(AppError::invalid_response);
    }
    let text = value.as_str().ok_or_else(AppError::invalid_response)?;
    let timestamp = OffsetDateTime::parse(text, &Rfc3339)
        .map_err(|_| AppError::invalid_response())?
        .unix_timestamp();
    u64::try_from(timestamp)
        .ok()
        .filter(|timestamp| *timestamp <= LATEST_PLAUSIBLE_DATE_SECS)
        .ok_or_else(AppError::invalid_response)
}

fn validate_outgoing(to: &str, subject: &str, body: &str) -> AppResult<()> {
    maild_client::validate_send_field(maild_client::SendField::To, to)
        .and_then(|_| maild_client::validate_send_field(maild_client::SendField::Subject, subject))
        .and_then(|_| maild_client::validate_send_field(maild_client::SendField::Body, body))
        .map_err(|_| AppError::invalid_input())
}

fn validate_quota_current(value: &Value) -> AppResult<()> {
    fn scalar_is_safe(value: &Value) -> bool {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) => true,
            Value::String(text) => {
                let prefix = crate::secret::api_key_prefix();
                text.len() <= 128
                    && !text.chars().any(char::is_control)
                    && !text.contains(&prefix)
            }
            _ => false,
        }
    }

    let safe = match value {
        Value::Object(fields) if fields.len() <= 32 => fields.iter().all(|(key, value)| {
            !key.is_empty()
                && key.len() <= 64
                && !key.chars().any(char::is_control)
                && scalar_is_safe(value)
        }),
        value => scalar_is_safe(value),
    };
    if !safe || serde_json::to_vec(value).map_or(true, |encoded| encoded.len() > 1024) {
        return Err(AppError::invalid_response());
    }
    Ok(())
}

impl TryFrom<MailboxWire> for Mailbox {
    type Error = AppError;

    fn try_from(value: MailboxWire) -> Result<Self, Self::Error> {
        let id = require_positive(value.id)?;
        let address = require_text(value.address)?;
        let address = canonicalize_address(&address).map_err(|_| AppError::invalid_response())?;
        Ok(Self {
            id,
            address,
            status: value.status,
            send_disabled: value.send_disabled,
        })
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: Option<T>,
}

#[derive(Deserialize)]
struct ErrorEnvelopeWire {
    error: Option<ErrorWire>,
}

#[derive(Deserialize)]
struct ErrorWire {
    code: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MailboxWire {
    id: Option<i64>,
    address: Option<String>,
    status: Option<i64>,
    send_disabled: Option<bool>,
}

#[derive(Deserialize)]
struct AvailabilityWire {
    available: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MailWire {
    id: Option<i64>,
    mailbox_id: Option<i64>,
    from_address: Option<String>,
    to_address: Option<String>,
    subject: Option<String>,
    body_text: Option<String>,
    received_at: Option<Value>,
}

#[derive(Deserialize)]
struct MailPageWire {
    data: Option<Vec<MailWire>>,
    pagination: Option<PaginationWire>,
}

#[derive(Deserialize)]
struct PaginationWire {
    total: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct QuotaWire {
    current: Option<Value>,
    storage: Option<StorageQuotaWire>,
    mailboxes: Option<MailboxQuotaWire>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StorageQuotaWire {
    used: Option<i64>,
    quota: Option<i64>,
    usage_percent: Option<i64>,
    quota_source: Option<String>,
}

#[derive(Deserialize)]
struct MailboxQuotaWire {
    count: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SendWire<'a> {
    mailbox_id: i64,
    to: Vec<&'a str>,
    subject: &'a str,
    body_text: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    struct MockResponse {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
        drop_without_response: bool,
        delay: Duration,
    }

    impl MockResponse {
        fn json(status: u16, body: &str) -> Self {
            Self {
                status,
                headers: vec![("Content-Type", "application/json".into())],
                body: body.as_bytes().to_vec(),
                drop_without_response: false,
                delay: Duration::ZERO,
            }
        }

        fn empty(status: u16) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: Vec::new(),
                drop_without_response: false,
                delay: Duration::ZERO,
            }
        }

        fn dropped() -> Self {
            Self {
                status: 0,
                headers: Vec::new(),
                body: Vec::new(),
                drop_without_response: true,
                delay: Duration::ZERO,
            }
        }

        fn delayed(delay: Duration) -> Self {
            Self {
                status: 200,
                headers: Vec::new(),
                body: br#"{"data":[]}"#.to_vec(),
                drop_without_response: false,
                delay,
            }
        }
    }

    struct CapturedRequest {
        request_line: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl CapturedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    async fn mock_server(
        responses: Vec<MockResponse>,
    ) -> (
        String,
        Arc<Mutex<Vec<CapturedRequest>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let captures = Arc::new(Mutex::new(Vec::new()));
        let server_captures = captures.clone();
        let task = tokio::spawn(async move {
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                server_captures.lock().unwrap().push(request);
                if response.drop_without_response {
                    continue;
                }
                if !response.delay.is_zero() {
                    tokio::time::sleep(response.delay).await;
                }
                let reason = match response.status {
                    200 => "OK",
                    201 => "Created",
                    302 => "Found",
                    429 => "Too Many Requests",
                    503 => "Service Unavailable",
                    _ => "Response",
                };
                let mut head = format!(
                    "HTTP/1.1 {} {}\r\nConnection: close\r\n",
                    response.status, reason
                );
                if !response
                    .headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                {
                    head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
                }
                for (name, value) in response.headers {
                    head.push_str(name);
                    head.push_str(": ");
                    head.push_str(&value);
                    head.push_str("\r\n");
                }
                head.push_str("\r\n");
                stream.write_all(head.as_bytes()).await.unwrap();
                stream.write_all(&response.body).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        (format!("http://{address}/openapi/v1"), captures, task)
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> CapturedRequest {
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 2048];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
            assert!(bytes.len() < 64 * 1024);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let head = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap().to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_owned(), value.trim().to_owned()))
            .collect();
        let content_length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        while bytes.len() - header_end < content_length {
            let mut chunk = [0_u8; 2048];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
        }
        CapturedRequest {
            request_line,
            headers,
            body: bytes[header_end..header_end + content_length].to_vec(),
        }
    }

    fn fake_secret() -> SecretBuffer {
        let mut key = crate::secret::api_key_prefix().into_bytes();
        key.extend(std::iter::repeat_n(b'a', 64));
        SecretBuffer::from_bytes(key).unwrap()
    }

    #[test]
    fn production_origin_cannot_be_replaced() {
        assert!(ApiClient::build(
            fake_secret(),
            "https://example.test",
            false,
            ProxySnapshot::default_system(),
        )
        .is_err());
        assert!(ApiClient::for_test(fake_secret(), "https://127.0.0.1").is_err());
        assert!(ApiClient::for_test(fake_secret(), "http://example.test").is_err());
    }

    #[test]
    fn optional_identity_fields_fail_closed() {
        let missing_id = MailboxWire {
            id: None,
            address: Some("box@seek.li".into()),
            status: None,
            send_disabled: None,
        };
        assert_eq!(
            Mailbox::try_from(missing_id).unwrap_err().code(),
            ErrorCode::InvalidResponse
        );
        let empty_address = MailboxWire {
            id: Some(1),
            address: Some(String::new()),
            status: None,
            send_disabled: None,
        };
        assert!(Mailbox::try_from(empty_address).is_err());
    }

    #[test]
    fn upstream_error_summary_is_bounded_sanitized_and_redacts_key_shapes() {
        let summary = upstream_error_summary(
            br#"{"error":{"code":"bad\ncode","message":"request\u0000 rejected"}}"#,
        )
        .unwrap();
        assert!(summary.len() < 320);
        assert!(!summary.chars().any(char::is_control));
        assert!(summary.contains("badcode"));

        let prefix = crate::secret::api_key_prefix();
        let leaked_payload =
            format!(r#"{{"error":{{"code":"bad_key","message":"{prefix}should_not_escape"}}}}"#);
        let redacted = upstream_error_summary(leaked_payload.as_bytes()).unwrap();
        assert_eq!(redacted, "NodeSeek Email 上游错误内容已隐藏");
        assert!(!redacted.contains(&prefix));
    }

    #[tokio::test]
    async fn empty_200_send_is_success_and_uses_exact_basic_shape() {
        let prefix = crate::secret::api_key_prefix();
        let expected_bearer = format!("Bearer {prefix}");
        let (origin, captures, server) = mock_server(vec![MockResponse::empty(200)]).await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        client
            .send_plain_text(41, "recipient@example.com", "subject", "body")
            .await
            .unwrap();
        server.await.unwrap();
        let requests = captures.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0]
            .request_line
            .starts_with("POST /openapi/v1/send "));
        assert!(requests[0]
            .header("authorization")
            .is_some_and(|value| value.starts_with(&expected_bearer)));
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["mailboxId"], 41);
        assert_eq!(body["to"].as_array().unwrap().len(), 1);
        assert_eq!(body["to"][0], "recipient@example.com");
        assert_eq!(body["subject"], "subject");
        assert_eq!(body["bodyText"], "body");
        assert!(body.get("bodyHtml").is_none());
        assert!(!requests[0].request_line.contains(&prefix));
        assert!(!requests[0]
            .body
            .windows(prefix.len())
            .any(|part| part == prefix.as_bytes()));
    }

    #[tokio::test]
    async fn mail_discovery_fetches_one_bounded_page_and_returns_next_offset() {
        let response = r#"{
            "data":[
                {"id":30,"mailboxId":41},
                {"id":10,"mailboxId":41},
                {"id":30,"mailboxId":41}
            ],
            "pagination":{"total":30,"limit":3,"offset":10}
        }"#;
        let (origin, captures, server) = mock_server(vec![MockResponse::json(200, response)]).await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        let page = client.remote_mail_ids_page(41, 10).await.unwrap();
        assert_eq!(page.ids, vec![10, 30]);
        assert_eq!(page.next_offset, 13, "必须服从服务端返回的分页窗口");
        server.await.unwrap();
        let requests = captures.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].request_line.contains("folder=inbox"));
        assert!(requests[0].request_line.contains("limit=10"));
        assert!(requests[0].request_line.contains("offset=10"));
    }

    #[tokio::test]
    async fn mail_discovery_skips_rows_from_other_mailboxes() {
        let response = r#"{
            "data":[
                {"id":30,"mailboxId":41},
                {"id":20,"mailboxId":99},
                {"id":10}
            ],
            "pagination":{"total":30,"limit":3,"offset":10}
        }"#;
        let (origin, _captures, server) =
            mock_server(vec![MockResponse::json(200, response)]).await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        let page = client.remote_mail_ids_page(41, 10).await.unwrap();
        assert_eq!(page.ids, vec![30]);
        assert_eq!(page.next_offset, 13);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mail_detail_accepts_live_nested_shape() {
        let response = r#"{
            "data":{"mail":{
                "id":10,
                "mailboxId":41,
                "fromAddress":"sender@example.test",
                "toAddress":"box@seek.li",
                "subject":"subject",
                "bodyText":"body",
                "receivedAt":"2026-08-23T00:00:00Z"
            }}
        }"#;
        let (origin, _captures, server) =
            mock_server(vec![MockResponse::json(200, response)]).await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        let mail = client.mail(41, 10).await.unwrap();
        assert_eq!(mail.id, 10);
        assert_eq!(mail.recipient, "box@seek.li");
        assert_eq!(mail.body_text, "body");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn quota_accepts_openapi_fields_and_bounded_live_current_shape() {
        let response = r#"{
            "data":{
                "current":{"mailboxes":21,"period":"weekly"},
                "storage":{"used":7,"quota":100,"usagePercent":7,"quotaSource":"rank"},
                "mailboxes":{"count":21}
            }
        }"#;
        let (origin, _captures, server) =
            mock_server(vec![MockResponse::json(200, response)]).await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        let quota = client.quota().await.unwrap();
        assert_eq!(quota.current.unwrap()["mailboxes"], 21);
        assert_eq!(quota.mailbox_count, Some(21));
        assert_eq!(quota.storage_quota_source.as_deref(), Some("rank"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn send_response_loss_is_delivery_unknown_and_never_retried() {
        let (origin, captures, server) = mock_server(vec![MockResponse::dropped()]).await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        let error = client
            .send_plain_text(41, "recipient@example.com", "subject", "body")
            .await
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::DeliveryUnknown);
        server.await.unwrap();
        assert_eq!(captures.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn create_response_loss_reconciles_once_without_second_post() {
        let response = r#"{"data":[{"id":41,"address":"box@seek.li"}]}"#;
        let (origin, captures, server) = mock_server(vec![
            MockResponse::dropped(),
            MockResponse::json(200, response),
        ])
        .await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        let outcome = client.create_mailbox("box@seek.li").await.unwrap();
        assert!(outcome.reconciled);
        assert_eq!(outcome.mailbox.id, 41);
        server.await.unwrap();
        let requests = captures.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0]
            .request_line
            .starts_with("POST /openapi/v1/mailboxes "));
        assert!(requests[1]
            .request_line
            .starts_with("GET /openapi/v1/mailboxes "));
    }

    #[tokio::test]
    async fn malformed_create_response_uses_exact_address_reconciliation() {
        let malformed = r#"{"data":{"address":"box@seek.li"}}"#;
        let reconciled = r#"{"data":[{"id":41,"address":"box@seek.li"}]}"#;
        let (origin, captures, server) = mock_server(vec![
            MockResponse::json(201, malformed),
            MockResponse::json(200, reconciled),
        ])
        .await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        assert!(
            client
                .create_mailbox("box@seek.li")
                .await
                .unwrap()
                .reconciled
        );
        server.await.unwrap();
        let requests = captures.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.request_line.starts_with("POST "))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn rate_limit_advances_gate_for_later_operations() {
        let mut response = MockResponse::json(429, r#"{"error":{"code":"limited"}}"#);
        response.headers.push(("Retry-After", "60".into()));
        let (origin, captures, server) = mock_server(vec![response]).await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        assert_eq!(
            client.list_mailboxes().await.unwrap_err().code(),
            ErrorCode::RateLimited
        );
        assert_eq!(
            client.quota().await.unwrap_err().code(),
            ErrorCode::RateLimited
        );
        server.await.unwrap();
        assert_eq!(captures.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn read_only_get_has_one_bounded_retry() {
        let ok = r#"{"data":[]}"#;
        let (origin, captures, server) = mock_server(vec![
            MockResponse::json(503, r#"{"error":{}}"#),
            MockResponse::json(200, ok),
        ])
        .await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        assert!(client.list_mailboxes().await.unwrap().is_empty());
        server.await.unwrap();
        assert_eq!(captures.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn read_retry_budget_is_one_total_deadline() {
        let (origin, captures, server) =
            mock_server(vec![MockResponse::delayed(Duration::from_secs(1))]).await;
        let client = ApiClient::for_test_with_read_timeout(
            fake_secret(),
            &origin,
            Duration::from_millis(75),
        )
        .unwrap();
        let started = tokio::time::Instant::now();
        assert_eq!(
            client.list_mailboxes().await.unwrap_err().code(),
            ErrorCode::Timeout
        );
        assert!(started.elapsed() < Duration::from_millis(300));
        assert_eq!(captures.lock().unwrap().len(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn redirect_is_not_followed() {
        let mut redirect = MockResponse::empty(302);
        redirect
            .headers
            .push(("Location", "http://127.0.0.1:9/credential-sink".into()));
        let (origin, captures, server) = mock_server(vec![redirect]).await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        assert_eq!(
            client.list_mailboxes().await.unwrap_err().code(),
            ErrorCode::InvalidResponse
        );
        server.await.unwrap();
        assert_eq!(captures.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn content_length_over_cap_is_rejected_before_body_read() {
        let mut response = MockResponse::empty(200);
        response
            .headers
            .push(("Content-Length", (MAX_RESPONSE_BYTES + 1).to_string()));
        let (origin, captures, server) = mock_server(vec![response]).await;
        let client = ApiClient::for_test(fake_secret(), &origin).unwrap();
        assert_eq!(
            client.list_mailboxes().await.unwrap_err().code(),
            ErrorCode::ResponseTooLarge
        );
        server.await.unwrap();
        assert_eq!(captures.lock().unwrap().len(), 1);
    }

    #[test]
    fn parse_received_at_rejects_millisecond_or_five_digit_year_timestamps() {
        assert_eq!(
            parse_received_at(Some(serde_json::json!(1_755_907_200_u64))).unwrap(),
            1_755_907_200
        );
        assert_eq!(
            parse_received_at(Some(serde_json::json!(253_402_300_799_u64))).unwrap(),
            253_402_300_799
        );
        assert_eq!(
            parse_received_at(Some(serde_json::json!(1_795_000_000_000_u64)))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidResponse
        );
        assert_eq!(
            parse_received_at(Some(serde_json::json!(253_402_300_800_u64)))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidResponse
        );
    }

    #[cfg(feature = "live-tests")]
    #[tokio::test]
    #[ignore = "requires explicit read-only live opt-in and a Keychain credential"]
    async fn live_read_only_list_and_quota() {
        use crate::secret::{PlatformSecretStore, SecretStore as _};

        if std::env::var("NODESEEK_LIVE_READ_ONLY").as_deref() != Ok("1") {
            return;
        }
        let client = ApiClient::production(PlatformSecretStore.load().unwrap()).unwrap();
        client.list_mailboxes().await.unwrap();
        client.quota().await.unwrap();
    }
}
