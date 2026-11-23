//! notify：有界系统通知服务。
//!
//! `Notifier::notify` 只做字段收束与 `try_send`，不会阻塞 cordis 事件循环。macOS
//! provider 由单一 worker 串行访问 `UNUserNotificationCenter`；查询授权不会弹窗，只有
//! `request_authorization`（由显式 API/UI 操作调用）才可能触发系统授权界面。

use std::any::TypeId;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{JoinHandle, Thread};
use std::time::Duration;

use cordis::{Context, Plugin, PluginError, Service};
pub use maild_client::{NotificationAuthorization, NotificationAvailability, NotificationStatus};
use serde::Deserialize;

use crate::frontend::{FrontendBroker, OpenIntent};

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::prepare_gui_session as prepare_macos_gui_session;

const QUEUE_CAPACITY: usize = 32;
const SENT_CAPACITY: usize = 64;
const MAX_TITLE_BYTES: usize = 256;
const MAX_BODY_BYTES: usize = 2_048;
const MAX_CONTROL_WAIT: Duration = Duration::from_secs(3);
const MAX_SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize, Default)]
pub struct NotifyConfig {
    /// "dev-log"（默认，测试用）| "macos"（Agent 原生系统通知）。
    #[serde(default)]
    pub provider: String,
}

impl Service for NotifyConfig {
    fn name(&self) -> &'static str {
        "notify.config"
    }
}

/// 一条已经提交给 Notifier 的有界通知（测试与健康检查用）。
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub target: Option<OpenIntent>,
}

pub(crate) struct ActivationSink {
    frontend: Mutex<Option<std::sync::Weak<FrontendBroker>>>,
}

impl ActivationSink {
    fn new() -> Self {
        Self {
            frontend: Mutex::new(None),
        }
    }

    fn attach(&self, broker: &Arc<FrontendBroker>) {
        *self.frontend.lock().unwrap() = Some(Arc::downgrade(broker));
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn activate(&self, intent: OpenIntent) {
        let broker = self
            .frontend
            .lock()
            .unwrap()
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        let Some(broker) = broker else {
            eprintln!("[maild/notify] frontend broker unavailable");
            return;
        };
        if !broker.open(intent).unwrap_or(false) {
            eprintln!("[maild/notify] frontend activation failed; intent remains queued");
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NotificationMetrics {
    pub submitted: u64,
    pub dropped: u64,
    pub failed: u64,
}

#[derive(Clone, Copy, Debug)]
struct DriverStatus {
    availability: NotificationAvailability,
    authorization: NotificationAuthorization,
    request_in_flight: bool,
}

trait PlatformDriver: 'static {
    fn query_authorization(&mut self) -> Result<DriverStatus, &'static str>;
    fn request_authorization(&mut self) -> Result<DriverStatus, &'static str>;
    fn deliver(&mut self, notification: &Notification) -> Result<(), &'static str>;

    fn poll_authorization(&mut self) -> Option<Result<DriverStatus, &'static str>> {
        None
    }
}

struct DevLogDriver;

impl PlatformDriver for DevLogDriver {
    fn query_authorization(&mut self) -> Result<DriverStatus, &'static str> {
        Ok(DriverStatus {
            availability: NotificationAvailability::Available,
            authorization: NotificationAuthorization::NotApplicable,
            request_in_flight: false,
        })
    }

    fn request_authorization(&mut self) -> Result<DriverStatus, &'static str> {
        self.query_authorization()
    }

    fn deliver(&mut self, notification: &Notification) -> Result<(), &'static str> {
        // 证明开发投递路径被调用，但不把验证码、标题或正文写入终端。
        eprintln!("[maild/notify] notification {} submitted", notification.id);
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
struct UnsupportedMacosDriver;

#[cfg(not(target_os = "macos"))]
impl PlatformDriver for UnsupportedMacosDriver {
    fn query_authorization(&mut self) -> Result<DriverStatus, &'static str> {
        Ok(DriverStatus {
            availability: NotificationAvailability::Unsupported,
            authorization: NotificationAuthorization::Unknown,
            request_in_flight: false,
        })
    }

    fn request_authorization(&mut self) -> Result<DriverStatus, &'static str> {
        self.query_authorization()
    }

    fn deliver(&mut self, _notification: &Notification) -> Result<(), &'static str> {
        Err("native notifications are unavailable")
    }
}

struct InvalidProviderDriver;

impl PlatformDriver for InvalidProviderDriver {
    fn query_authorization(&mut self) -> Result<DriverStatus, &'static str> {
        Ok(DriverStatus {
            availability: NotificationAvailability::Unsupported,
            authorization: NotificationAuthorization::Unknown,
            request_in_flight: false,
        })
    }

    fn request_authorization(&mut self) -> Result<DriverStatus, &'static str> {
        self.query_authorization()
    }

    fn deliver(&mut self, _notification: &Notification) -> Result<(), &'static str> {
        Err("notification provider is invalid")
    }
}

#[derive(Clone, Copy, Debug)]
struct RuntimeState {
    availability: NotificationAvailability,
    authorization: NotificationAuthorization,
    request_in_flight: bool,
    fresh: bool,
}

impl RuntimeState {
    fn checking(provider: &str) -> Self {
        if provider == "dev-log" {
            Self {
                availability: NotificationAvailability::Available,
                authorization: NotificationAuthorization::NotApplicable,
                request_in_flight: false,
                fresh: true,
            }
        } else {
            Self {
                availability: NotificationAvailability::Available,
                authorization: NotificationAuthorization::Checking,
                request_in_flight: false,
                fresh: false,
            }
        }
    }
}

struct Shared {
    runtime: Mutex<RuntimeState>,
    authorization_request: AtomicBool,
    queue_depth: AtomicU64,
    submitted: AtomicU64,
    dropped: AtomicU64,
    failed: AtomicU64,
    stopping: AtomicBool,
}

enum ControlCommand {
    Refresh(SyncSender<Result<(), &'static str>>),
    RequestAuthorization(SyncSender<Result<(), &'static str>>),
}

/// 系统通知服务（spine 服务之一；provider 由配置选，插件可换）。
pub struct Notifier {
    provider: String,
    deliveries: Option<SyncSender<Notification>>,
    controls: Option<SyncSender<ControlCommand>>,
    shared: Arc<Shared>,
    sent: Mutex<VecDeque<Notification>>,
    next_id: AtomicU64,
    worker_waker: Thread,
    worker: Mutex<Option<JoinHandle<()>>>,
    worker_done: Mutex<Option<Receiver<()>>>,
    activation: Arc<ActivationSink>,
}

impl Service for Notifier {
    fn name(&self) -> &'static str {
        "notifier"
    }
}

impl Notifier {
    pub fn new(provider: &str) -> Result<Notifier, String> {
        let provider = normalize_provider(provider).to_string();
        Self::with_driver_factory(provider, make_driver)
    }

    fn with_driver_factory<F>(provider: String, factory: F) -> Result<Notifier, String>
    where
        F: FnOnce(&str, Arc<ActivationSink>) -> Box<dyn PlatformDriver> + Send + 'static,
    {
        let (delivery_tx, delivery_rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (control_tx, control_rx) = mpsc::sync_channel(4);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            runtime: Mutex::new(RuntimeState::checking(&provider)),
            authorization_request: AtomicBool::new(false),
            queue_depth: AtomicU64::new(0),
            submitted: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
        });
        let worker_shared = shared.clone();
        let worker_provider = provider.clone();
        let activation = Arc::new(ActivationSink::new());
        let worker_activation = activation.clone();
        let worker = std::thread::Builder::new()
            .name("maild-notify".into())
            .spawn(move || {
                let driver = platform_call(|| factory(&worker_provider, worker_activation));
                worker_loop(driver, delivery_rx, control_rx, &worker_shared);
                let _ = done_tx.try_send(());
            })
            .map_err(|error| format!("notification worker thread creation failed: {error}"))?;
        let worker_waker = worker.thread().clone();
        Ok(Notifier {
            provider,
            deliveries: Some(delivery_tx),
            controls: Some(control_tx),
            shared,
            sent: Mutex::new(VecDeque::with_capacity(SENT_CAPACITY)),
            next_id: AtomicU64::new(1),
            worker_waker,
            worker: Mutex::new(Some(worker)),
            worker_done: Mutex::new(Some(done_rx)),
            activation,
        })
    }

    /// 非阻塞提交通知。字段、队列和测试日志均有硬上限；满队列直接丢弃。
    pub fn notify(&self, title: &str, body: &str) {
        self.notify_with_target(title, body, None);
    }

    pub fn notify_with_target(&self, title: &str, body: &str, target: Option<OpenIntent>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let notification = Notification {
            id,
            title: bounded_text(title, MAX_TITLE_BYTES, maild_brand::PRODUCT_NAME),
            body: bounded_text(body, MAX_BODY_BYTES, ""),
            target: target.filter(|intent| !matches!(intent, OpenIntent::Unknown)),
        };
        if cfg!(test) || self.provider == "dev-log" {
            let mut sent = self.sent.lock().unwrap();
            if sent.len() == SENT_CAPACITY {
                sent.pop_front();
            }
            sent.push_back(notification.clone());
        }
        let authorized = {
            let runtime = *self.shared.runtime.lock().unwrap();
            can_deliver(DriverStatus {
                availability: runtime.availability,
                authorization: runtime.authorization,
                request_in_flight: runtime.request_in_flight,
            })
        };
        if self.shared.stopping.load(Ordering::Acquire) || !authorized {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.shared.queue_depth.fetch_add(1, Ordering::Relaxed);
        match self
            .deliveries
            .as_ref()
            .expect("worker sender exists before drop")
            .try_send(notification)
        {
            Ok(()) => self.worker_waker.unpark(),
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.shared.queue_depth.fetch_sub(1, Ordering::Relaxed);
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn attach_frontend(&self, broker: &Arc<FrontendBroker>) {
        self.activation.attach(broker);
    }

    /// 查询当前系统授权。该操作不会请求权限，也不会弹出系统界面。
    pub fn refresh_status(&self) -> Result<NotificationStatus, &'static str> {
        if let Err(error) = self.control(false) {
            mark_stale(&self.shared);
            return Err(error);
        }
        Ok(self.status())
    }

    /// 显式请求授权。只有 `not_determined` 才调用系统请求；其他状态只刷新并返回。
    pub fn request_authorization(&self) -> Result<NotificationStatus, &'static str> {
        if !self.begin_authorization_request() {
            return Ok(self.status());
        }
        if let Err(error) = self.control(true) {
            self.shared
                .authorization_request
                .store(false, Ordering::Release);
            set_request_in_flight(&self.shared, false);
            return Err(error);
        }
        Ok(self.status())
    }

    pub fn status(&self) -> NotificationStatus {
        let runtime = *self.shared.runtime.lock().unwrap();
        NotificationStatus {
            provider: self.provider.clone(),
            availability: runtime.availability,
            authorization: runtime.authorization,
            request_in_flight: runtime.request_in_flight,
            fresh: runtime.fresh,
        }
    }

    /// 最近提交的有界测试记录，最近在前。
    pub fn sent(&self) -> Vec<Notification> {
        self.sent.lock().unwrap().iter().rev().cloned().collect()
    }

    pub fn clear(&self) {
        self.sent.lock().unwrap().clear();
    }

    pub fn metrics(&self) -> NotificationMetrics {
        NotificationMetrics {
            submitted: self.shared.submitted.load(Ordering::Relaxed),
            dropped: self.shared.dropped.load(Ordering::Relaxed),
            failed: self.shared.failed.load(Ordering::Relaxed),
        }
    }

    /// 停止接收新通知并等待 worker 有界退出。系统通知是瞬时信号，停机时队列直接丢弃。
    pub async fn shutdown(&self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.worker_waker.unpark();
        let receiver = self.worker_done.lock().unwrap().take();
        let Some(receiver) = receiver else {
            return;
        };
        let finished =
            tokio::task::spawn_blocking(move || receiver.recv_timeout(MAX_SHUTDOWN_WAIT).is_ok())
                .await
                .unwrap_or(false);
        let worker = self.worker.lock().unwrap().take();
        if finished {
            if let Some(worker) = worker {
                let _ = tokio::task::spawn_blocking(move || worker.join()).await;
            }
        }
        self.shared.queue_depth.store(0, Ordering::Relaxed);
    }

    fn control(&self, request_authorization: bool) -> Result<(), &'static str> {
        if self.shared.stopping.load(Ordering::Acquire) {
            return Err("notification worker is stopping");
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let command = if request_authorization {
            ControlCommand::RequestAuthorization(reply_tx)
        } else {
            ControlCommand::Refresh(reply_tx)
        };
        self.controls
            .as_ref()
            .ok_or("notification worker is unavailable")?
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => "notification worker is busy",
                TrySendError::Disconnected(_) => "notification worker is unavailable",
            })?;
        self.worker_waker.unpark();
        reply_rx
            .recv_timeout(MAX_CONTROL_WAIT)
            .map_err(|_| "notification worker timed out")?
    }

    fn begin_authorization_request(&self) -> bool {
        if self
            .shared
            .authorization_request
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let mut runtime = self.shared.runtime.lock().unwrap();
        runtime.request_in_flight = true;
        if runtime.availability == NotificationAvailability::Available
            && matches!(
                runtime.authorization,
                NotificationAuthorization::Checking | NotificationAuthorization::NotDetermined
            )
        {
            runtime.authorization = NotificationAuthorization::Checking;
        }
        true
    }
}

impl Drop for Notifier {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.worker_waker.unpark();
        // Never block in Drop. The plugin disposer calls `shutdown`; direct users detach the
        // worker, which observes `stopping` before starting another platform operation.
        self.worker.get_mut().unwrap().take();
        self.worker_done.get_mut().unwrap().take();
    }
}

fn normalize_provider(provider: &str) -> &str {
    match provider.trim() {
        "" => "dev-log",
        value => value,
    }
}

pub(crate) fn validate_config(config: &NotifyConfig) -> Result<(), String> {
    match normalize_provider(&config.provider) {
        "dev-log" | "macos" => Ok(()),
        provider => Err(format!("notify provider 不支持: {provider}")),
    }
}

fn make_driver(provider: &str, activation: Arc<ActivationSink>) -> Box<dyn PlatformDriver> {
    #[cfg(not(target_os = "macos"))]
    let _ = &activation;
    match provider {
        "dev-log" => Box::new(DevLogDriver),
        "macos" => {
            #[cfg(target_os = "macos")]
            {
                Box::new(macos::MacosDriver::new(activation))
            }
            #[cfg(not(target_os = "macos"))]
            {
                Box::new(UnsupportedMacosDriver)
            }
        }
        _ => Box::new(InvalidProviderDriver),
    }
}

fn worker_loop(
    mut driver: Box<dyn PlatformDriver>,
    deliveries: Receiver<Notification>,
    controls: Receiver<ControlCommand>,
    shared: &Shared,
) {
    let initial = platform_call(|| driver.query_authorization());
    let _ = update_from_query(initial, shared);
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        if let Some(result) = platform_call(|| driver.poll_authorization()) {
            if update_from_query(result, shared).is_err() {
                set_request_in_flight(shared, false);
                shared.failed.fetch_add(1, Ordering::Relaxed);
            }
            shared.authorization_request.store(false, Ordering::Release);
        }
        match controls.try_recv() {
            Ok(command) => {
                platform_call(|| handle_control(command, driver.as_mut(), shared));
                continue;
            }
            Err(TryRecvError::Disconnected) if shared.stopping.load(Ordering::Acquire) => break,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
        match deliveries.try_recv() {
            Ok(notification) => {
                shared.queue_depth.fetch_sub(1, Ordering::Relaxed);
                let status = platform_call(|| driver.query_authorization());
                match status {
                    Ok(status) if can_deliver(status) => {
                        install_status(status, shared, true);
                        if shared.stopping.load(Ordering::Acquire) {
                            shared.dropped.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        match platform_call(|| driver.deliver(&notification)) {
                            Ok(()) => {
                                shared.submitted.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(_) => {
                                shared.failed.fetch_add(1, Ordering::Relaxed);
                                mark_stale(shared);
                            }
                        }
                    }
                    Ok(status) => {
                        install_status(status, shared, true);
                        shared.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        shared.failed.fetch_add(1, Ordering::Relaxed);
                        mark_stale(shared);
                    }
                }
            }
            Err(TryRecvError::Disconnected) if shared.stopping.load(Ordering::Acquire) => break,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                std::thread::park();
            }
        }
    }
}

fn platform_call<T>(call: impl FnOnce() -> T) -> T {
    #[cfg(target_os = "macos")]
    {
        objc2::rc::autoreleasepool(|_| call())
    }
    #[cfg(not(target_os = "macos"))]
    {
        call()
    }
}

fn handle_control(command: ControlCommand, driver: &mut dyn PlatformDriver, shared: &Shared) {
    match command {
        ControlCommand::Refresh(reply) => {
            let result = driver.query_authorization();
            let reply_result = update_from_query(result, shared);
            let _ = reply.try_send(reply_result);
        }
        ControlCommand::RequestAuthorization(reply) => {
            let result = request_if_needed(driver, shared);
            if result.is_err() {
                mark_stale(shared);
                set_request_in_flight(shared, false);
            }
            if result.is_err() || !shared.runtime.lock().unwrap().request_in_flight {
                shared.authorization_request.store(false, Ordering::Release);
            }
            let _ = reply.try_send(result);
        }
    }
}

fn request_if_needed(driver: &mut dyn PlatformDriver, shared: &Shared) -> Result<(), &'static str> {
    let current = driver.query_authorization()?;
    install_status(current, shared, true);
    if current.availability != NotificationAvailability::Available
        || current.authorization != NotificationAuthorization::NotDetermined
    {
        return Ok(());
    }
    if shared.stopping.load(Ordering::Acquire) {
        return Err("notification worker is stopping");
    }
    let requested = driver.request_authorization()?;
    install_status(requested, shared, true);
    let refreshed = driver.query_authorization()?;
    install_status(refreshed, shared, true);
    Ok(())
}

fn update_from_query(
    result: Result<DriverStatus, &'static str>,
    shared: &Shared,
) -> Result<(), &'static str> {
    match result {
        Ok(status) => {
            install_status(status, shared, true);
            Ok(())
        }
        Err(error) => {
            mark_stale(shared);
            Err(error)
        }
    }
}

fn install_status(status: DriverStatus, shared: &Shared, fresh: bool) {
    let mut runtime = shared.runtime.lock().unwrap();
    runtime.availability = status.availability;
    runtime.authorization = status.authorization;
    runtime.request_in_flight = status.request_in_flight;
    runtime.fresh = fresh;
}

fn mark_stale(shared: &Shared) {
    shared.runtime.lock().unwrap().fresh = false;
}

fn set_request_in_flight(shared: &Shared, in_flight: bool) {
    shared.runtime.lock().unwrap().request_in_flight = in_flight;
}

fn can_deliver(status: DriverStatus) -> bool {
    status.availability == NotificationAvailability::Available
        && matches!(
            status.authorization,
            NotificationAuthorization::Authorized
                | NotificationAuthorization::Provisional
                | NotificationAuthorization::Ephemeral
                | NotificationAuthorization::NotApplicable
        )
}

fn bounded_text(value: &str, max_bytes: usize, fallback: &str) -> String {
    let mut output = String::with_capacity(value.len().min(max_bytes));
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if output.len().saturating_add(character.len_utf8()) > max_bytes {
            break;
        }
        output.push(character);
    }
    let output = output.trim();
    if output.is_empty() {
        fallback.to_string()
    } else {
        output.to_string()
    }
}

/// notify 插件：inject 配置 → provide Notifier。
pub struct NotifyPlugin;

impl Plugin for NotifyPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<NotifyConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let cfg = ctx.get::<NotifyConfig>().expect("inject 就绪");
        validate_config(&cfg)?;
        let notifier = Arc::new(Notifier::new(&cfg.provider)?);
        ctx.provide::<Notifier>(notifier.clone());
        if let Some(frontend) = ctx.get::<FrontendBroker>() {
            notifier.attach_frontend(&frontend);
        }
        ctx.effect_async(async move {
            notifier.shutdown().await;
        });
        Ok(())
    }
}

/// 此刻成文系统通知该用的内核文案（ADR-0048 决策 6）。
///
/// 每条通知**成文时**调用一次：读 daemon 的语言偏好 `effective` 列表与 `maild-l10n` 协商；
/// `system` 模式下 `effective` 会重读系统语言，所以切换偏好或系统语言后下一条通知即跟随。
/// 上下文里没有偏好服务（未从配置文件装配、单元测试）时按 `FALLBACK_LANGUAGE` 协商，即简体中文。
pub fn kernel_messages(ctx: &Context) -> &'static maild_l10n::Messages {
    let effective = ctx
        .get::<crate::locale_preference::LocalePreference>()
        .map(|preference| preference.effective())
        .unwrap_or_else(|| vec![crate::locale_preference::FALLBACK_LANGUAGE.to_string()]);
    maild_l10n::negotiate(&effective).messages()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    struct MockDriver {
        authorization: NotificationAuthorization,
        requests: Arc<AtomicUsize>,
        deliveries: Arc<AtomicUsize>,
        delay: Duration,
    }

    struct PendingAuthorizationDriver {
        requests: Arc<AtomicUsize>,
        completed: Arc<AtomicBool>,
        polls: Arc<AtomicUsize>,
    }

    struct SlowQueryDriver {
        query_started: Arc<AtomicBool>,
        deliveries: Arc<AtomicUsize>,
        delay: Duration,
    }

    struct IdleDriver {
        queries: Arc<AtomicUsize>,
    }

    impl PlatformDriver for IdleDriver {
        fn query_authorization(&mut self) -> Result<DriverStatus, &'static str> {
            self.queries.fetch_add(1, Ordering::Relaxed);
            Ok(DriverStatus {
                availability: NotificationAvailability::Available,
                authorization: NotificationAuthorization::Authorized,
                request_in_flight: false,
            })
        }

        fn request_authorization(&mut self) -> Result<DriverStatus, &'static str> {
            self.query_authorization()
        }

        fn deliver(&mut self, _notification: &Notification) -> Result<(), &'static str> {
            Ok(())
        }
    }

    impl PlatformDriver for SlowQueryDriver {
        fn query_authorization(&mut self) -> Result<DriverStatus, &'static str> {
            self.query_started.store(true, Ordering::Release);
            std::thread::sleep(self.delay);
            Ok(DriverStatus {
                availability: NotificationAvailability::Available,
                authorization: NotificationAuthorization::Authorized,
                request_in_flight: false,
            })
        }

        fn request_authorization(&mut self) -> Result<DriverStatus, &'static str> {
            self.query_authorization()
        }

        fn deliver(&mut self, _notification: &Notification) -> Result<(), &'static str> {
            self.deliveries.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    impl PlatformDriver for PendingAuthorizationDriver {
        fn query_authorization(&mut self) -> Result<DriverStatus, &'static str> {
            let requested = self.requests.load(Ordering::Relaxed) != 0;
            let completed = self.completed.load(Ordering::Acquire);
            Ok(DriverStatus {
                availability: NotificationAvailability::Available,
                authorization: if completed {
                    NotificationAuthorization::Authorized
                } else if requested {
                    NotificationAuthorization::Checking
                } else {
                    NotificationAuthorization::NotDetermined
                },
                request_in_flight: requested && !completed,
            })
        }

        fn request_authorization(&mut self) -> Result<DriverStatus, &'static str> {
            self.requests.fetch_add(1, Ordering::Relaxed);
            self.query_authorization()
        }

        fn deliver(&mut self, _notification: &Notification) -> Result<(), &'static str> {
            Ok(())
        }

        fn poll_authorization(&mut self) -> Option<Result<DriverStatus, &'static str>> {
            self.polls.fetch_add(1, Ordering::Relaxed);
            self.completed
                .load(Ordering::Acquire)
                .then(|| self.query_authorization())
        }
    }

    impl PlatformDriver for MockDriver {
        fn query_authorization(&mut self) -> Result<DriverStatus, &'static str> {
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            Ok(DriverStatus {
                availability: NotificationAvailability::Available,
                authorization: self.authorization,
                request_in_flight: false,
            })
        }

        fn request_authorization(&mut self) -> Result<DriverStatus, &'static str> {
            self.requests.fetch_add(1, Ordering::Relaxed);
            self.authorization = NotificationAuthorization::Authorized;
            self.query_authorization()
        }

        fn deliver(&mut self, _notification: &Notification) -> Result<(), &'static str> {
            self.deliveries.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn notifier_with_mock(
        authorization: NotificationAuthorization,
        delay: Duration,
    ) -> (Notifier, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let deliveries = Arc::new(AtomicUsize::new(0));
        let request_counter = requests.clone();
        let delivery_counter = deliveries.clone();
        let notifier = Notifier::with_driver_factory("macos".into(), move |_, _| {
            Box::new(MockDriver {
                authorization,
                requests: request_counter,
                deliveries: delivery_counter,
                delay,
            })
        })
        .unwrap();
        (notifier, requests, deliveries)
    }

    #[test]
    fn explicit_authorization_is_idempotent_and_delivery_never_prompts() {
        let (notifier, requests, deliveries) =
            notifier_with_mock(NotificationAuthorization::NotDetermined, Duration::ZERO);

        notifier.notify("OTP", "123456");
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(requests.load(Ordering::Relaxed), 0);
        assert_eq!(deliveries.load(Ordering::Relaxed), 0);

        let status = notifier.request_authorization().unwrap();
        assert_eq!(status.authorization, NotificationAuthorization::Authorized);
        notifier.request_authorization().unwrap();
        assert_eq!(requests.load(Ordering::Relaxed), 1);

        notifier.notify("OTP", "123456");
        for _ in 0..50 {
            if deliveries.load(Ordering::Relaxed) == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(deliveries.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn pending_human_authorization_is_single_flight_without_a_deadline() {
        let requests = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let driver_requests = requests.clone();
        let driver_completed = completed.clone();
        let driver_polls = polls.clone();
        let notifier = Notifier::with_driver_factory("macos".into(), move |_, _| {
            Box::new(PendingAuthorizationDriver {
                requests: driver_requests,
                completed: driver_completed,
                polls: driver_polls,
            })
        })
        .unwrap();

        let first = notifier.request_authorization().unwrap();
        assert!(first.request_in_flight);
        assert_eq!(first.authorization, NotificationAuthorization::Checking);
        let second = notifier.request_authorization().unwrap();
        assert!(second.request_in_flight);
        assert_eq!(requests.load(Ordering::Relaxed), 1);

        std::thread::sleep(Duration::from_millis(20));
        let idle_polls = polls.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(polls.load(Ordering::Relaxed), idle_polls);

        completed.store(true, Ordering::Release);
        notifier.worker_waker.unpark();
        for _ in 0..100 {
            if notifier.status().authorization == NotificationAuthorization::Authorized {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let final_status = notifier.status();
        assert_eq!(
            final_status.authorization,
            NotificationAuthorization::Authorized
        );
        assert!(!final_status.request_in_flight);
    }

    #[test]
    fn drop_never_waits_for_a_platform_callback() {
        let query_started = Arc::new(AtomicBool::new(false));
        let driver_started = query_started.clone();
        let notifier = Notifier::with_driver_factory("macos".into(), move |_, _| {
            Box::new(SlowQueryDriver {
                query_started: driver_started,
                deliveries: Arc::new(AtomicUsize::new(0)),
                delay: Duration::from_millis(500),
            })
        })
        .unwrap();
        for _ in 0..100 {
            if query_started.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(query_started.load(Ordering::Acquire));
        let started = std::time::Instant::now();
        drop(notifier);
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn idle_worker_is_event_driven() {
        let queries = Arc::new(AtomicUsize::new(0));
        let driver_queries = queries.clone();
        let notifier = Notifier::with_driver_factory("macos".into(), move |_, _| {
            Box::new(IdleDriver {
                queries: driver_queries,
            })
        })
        .unwrap();
        for _ in 0..100 {
            if notifier.status().fresh {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let idle_count = queries.load(Ordering::Relaxed);
        assert_eq!(idle_count, 1);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(queries.load(Ordering::Relaxed), idle_count);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_does_not_start_delivery_after_an_inflight_query() {
        let query_started = Arc::new(AtomicBool::new(false));
        let deliveries = Arc::new(AtomicUsize::new(0));
        let driver_started = query_started.clone();
        let driver_deliveries = deliveries.clone();
        let notifier = Notifier::with_driver_factory("macos".into(), move |_, _| {
            Box::new(SlowQueryDriver {
                query_started: driver_started,
                deliveries: driver_deliveries,
                delay: Duration::from_millis(150),
            })
        })
        .unwrap();
        while !notifier.status().fresh {
            tokio::task::yield_now().await;
        }
        query_started.store(false, Ordering::Release);
        notifier.notify("OTP", "123456");
        for _ in 0..100 {
            if query_started.load(Ordering::Acquire) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(query_started.load(Ordering::Acquire));

        notifier.shutdown().await;
        assert_eq!(deliveries.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn queue_fields_and_memory_log_are_bounded() {
        let (notifier, _, _) = notifier_with_mock(
            NotificationAuthorization::Authorized,
            Duration::from_millis(20),
        );
        notifier.refresh_status().unwrap();
        let title = format!("{}\nsecret", "界".repeat(200));
        let body = "x".repeat(MAX_BODY_BYTES * 2);
        for _ in 0..10_000 {
            notifier.notify(&title, &body);
        }
        let sent = notifier.sent();
        assert_eq!(sent.len(), SENT_CAPACITY);
        assert!(sent
            .iter()
            .all(|entry| entry.title.len() <= MAX_TITLE_BYTES));
        assert!(sent.iter().all(|entry| entry.body.len() <= MAX_BODY_BYTES));
        assert!(sent.iter().all(|entry| !entry.title.contains('\n')));
        assert!(notifier.metrics().dropped > 0);
    }

    #[test]
    fn denied_authorization_drops_without_request_or_delivery() {
        let (notifier, requests, deliveries) =
            notifier_with_mock(NotificationAuthorization::Denied, Duration::ZERO);
        let status = notifier.request_authorization().unwrap();
        assert_eq!(status.authorization, NotificationAuthorization::Denied);
        notifier.notify("OTP", "123456");
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(requests.load(Ordering::Relaxed), 0);
        assert_eq!(deliveries.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn unknown_provider_is_rejected() {
        assert!(validate_config(&NotifyConfig {
            provider: "osascript".into(),
        })
        .is_err());
    }
}
