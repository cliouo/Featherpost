use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use maild_client::{
    ClientError, ErrorKind, Incoming, JsonRpcError, RpcClient, ServerNotification,
    StreamConnection, StreamWriter,
};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::{
    AccountsResult, ActorOptions, AppEvent, ConnectionPhase, IdentityFailure,
    MessageSummariesResult, MessageSummary, StatusResult, ViewState,
};

const MAX_BUFFERED_NOTIFICATIONS: usize = 256;
const MAX_BUFFERED_NOTIFICATION_BYTES: usize = 2 * 1024 * 1024;
const MAX_NOTIFICATION_TEXT_BYTES: usize = 8 * 1024;
const MAX_NOTIFICATION_BODY_BYTES: usize = 64 * 1024;
const MAX_ACCOUNTS: usize = 256;
const MAX_PENDING_REQUESTS: usize = 64;
const MAX_REFRESH_WAITERS: usize = 64;
const MAX_IMMEDIATE_REFRESHES: usize = 1;
/// ADR-0048：daemon 规范化后最多 8 项、每项不超过 35 字节；这里留余量，只防失控的对端。
const MAX_LOCALE_TAGS: usize = 32;
const MAX_LOCALE_TAG_BYTES: usize = 64;
/// 快照一次发出的请求数：status、listAccounts、listMessageSummaries、getLocalePreference。
const SNAPSHOT_REQUESTS: usize = 4;

type RefreshWaiter = oneshot::Sender<Result<(), ClientError>>;

pub(crate) enum Command {
    Request {
        method: String,
        params: Value,
        deadline: Instant,
        reply: oneshot::Sender<Result<Value, ClientError>>,
    },
    Refresh {
        deadline: Instant,
        reply: RefreshWaiter,
    },
}

struct Actor {
    client: Arc<RpcClient>,
    options: ActorOptions,
    commands: mpsc::Receiver<Command>,
    state_tx: watch::Sender<ViewState>,
    events: broadcast::Sender<AppEvent>,
    shutdown: watch::Receiver<bool>,
    state: ViewState,
}

enum LoopControl {
    Continue,
    Stop,
}

enum SessionExit {
    Disconnected {
        error: ClientError,
        synchronized: bool,
    },
    Stop,
}

enum PendingKind {
    User(oneshot::Sender<Result<Value, ClientError>>),
    Snapshot(SnapshotPart),
}

struct PendingRequest {
    deadline: Instant,
    kind: PendingKind,
}

#[derive(Clone, Copy)]
enum SnapshotPart {
    Status,
    Accounts,
    Messages,
    Locale,
}

struct SnapshotTracker {
    status: Option<Result<Value, ClientError>>,
    accounts: Option<Result<Value, ClientError>>,
    messages: Option<Result<Value, ClientError>>,
    locale: Option<Result<Value, ClientError>>,
    /// 快照进行中收到了 `preferences/locale`：推送比快照里的 `getLocalePreference` 应答更新，
    /// 收尾时不再用应答覆盖它。
    locale_pushed: bool,
    buffered: VecDeque<ServerNotification>,
    buffered_bytes: usize,
    overflowed: bool,
    waiters: Vec<RefreshWaiter>,
}

struct SnapshotOutcome {
    follow_up: bool,
    waiters: Vec<RefreshWaiter>,
}

impl SnapshotTracker {
    fn new() -> Self {
        Self {
            status: None,
            accounts: None,
            messages: None,
            locale: None,
            locale_pushed: false,
            buffered: VecDeque::new(),
            buffered_bytes: 0,
            overflowed: false,
            waiters: Vec::new(),
        }
    }

    fn record(&mut self, part: SnapshotPart, result: Result<Value, ClientError>) {
        match part {
            SnapshotPart::Status => self.status = Some(result),
            SnapshotPart::Accounts => self.accounts = Some(result),
            SnapshotPart::Messages => self.messages = Some(result),
            SnapshotPart::Locale => self.locale = Some(result),
        }
    }

    fn is_complete(&self) -> bool {
        self.status.is_some()
            && self.accounts.is_some()
            && self.messages.is_some()
            && self.locale.is_some()
    }

    fn buffer(&mut self, notification: ServerNotification) {
        if matches!(notification, ServerNotification::Unknown(_)) {
            return;
        }
        let bytes = notification_bytes(&notification);
        while self.buffered.len() == MAX_BUFFERED_NOTIFICATIONS
            || self.buffered_bytes.saturating_add(bytes) > MAX_BUFFERED_NOTIFICATION_BYTES
        {
            let Some(dropped) = self.buffered.pop_front() else {
                break;
            };
            self.buffered_bytes = self
                .buffered_bytes
                .saturating_sub(notification_bytes(&dropped));
            self.overflowed = true;
        }
        if bytes > MAX_BUFFERED_NOTIFICATION_BYTES {
            self.overflowed = true;
            return;
        }
        self.buffered_bytes = self.buffered_bytes.saturating_add(bytes);
        self.buffered.push_back(notification);
    }
}

pub(crate) async fn run(
    client: RpcClient,
    options: ActorOptions,
    commands: mpsc::Receiver<Command>,
    state_tx: watch::Sender<ViewState>,
    events: broadcast::Sender<AppEvent>,
    shutdown: watch::Receiver<bool>,
) {
    let state = state_tx.borrow().clone();
    let mut actor = Actor {
        client: Arc::new(client),
        options,
        commands,
        state_tx,
        events,
        shutdown,
        state,
    };
    if actor.client.endpoint().supports_notifications() {
        actor.run_ipc().await;
    } else {
        actor.run_http().await;
    }
    actor.stop();
}

impl Actor {
    fn publish(&mut self) {
        self.state.revision = self.state.revision.saturating_add(1);
        self.state_tx.send_replace(self.state.clone());
    }

    fn set_phase(&mut self, phase: ConnectionPhase) {
        self.state.phase = phase;
        self.publish();
    }

    fn set_error(&mut self, error: &ClientError) {
        self.state.last_error = Some(error.to_string());
    }

    fn stop(&mut self) {
        if self.state.phase != ConnectionPhase::Stopped {
            self.state.phase = ConnectionPhase::Stopped;
            self.publish();
        }
    }

    fn shutdown_requested(&self) -> bool {
        *self.shutdown.borrow()
    }

    async fn run_ipc(&mut self) {
        let mut attempt = 0_u32;
        loop {
            self.set_phase(ConnectionPhase::Connecting { attempt });
            let client = self.client.clone();
            let connection = client.connect_stream();
            tokio::pin!(connection);
            let mut shutdown = self.shutdown.clone();
            let connection = loop {
                tokio::select! {
                    biased;
                    _ = wait_for_shutdown(&mut shutdown) => break None,
                    result = &mut connection => break Some(result),
                    command = self.commands.recv() => {
                        match self.handle_offline_command(command, "连接尚未建立").await {
                            LoopControl::Continue => {}
                            LoopControl::Stop => break None,
                        }
                    }
                }
            };
            let Some(connection) = connection else {
                return;
            };
            let mut connection = match connection {
                Ok(connection) => connection,
                Err(error) => {
                    self.set_error(&error);
                    attempt = attempt.saturating_add(1);
                    if matches!(self.wait_retry(attempt).await, LoopControl::Stop) {
                        return;
                    }
                    continue;
                }
            };

            // ADR-0030 决策 6：身份绑定发生在连接建立后、第一个领域请求前。失败按连接失败
            // 退避——用户可能正在安装前端包——但原因单独落 identity_failure，UI 据此给出
            // 「去安装」而不是「等网络」。
            if let Some(identity) = self.options.identity.clone() {
                let mut shutdown = self.shutdown.clone();
                let bound = await_or_shutdown(
                    &mut shutdown,
                    tokio::time::timeout(
                        self.options.request_timeout,
                        bind_identity(&client, &mut connection, &identity),
                    ),
                )
                .await;
                let Some(bound) = bound else {
                    return;
                };
                let bound = bound
                    .map_err(|_| {
                        (
                            IdentityFailure::Unavailable,
                            ClientError::connection("前端身份绑定超时"),
                        )
                    })
                    .and_then(|result| result);
                match bound {
                    Ok(principal) => {
                        self.state.principal = Some(principal);
                        self.state.identity_failure = None;
                        self.state.identity_failure_code = None;
                        self.publish();
                    }
                    Err((code, error)) => {
                        self.state.principal = None;
                        self.state.identity_failure = Some(error.message.clone());
                        self.state.identity_failure_code = Some(code);
                        self.set_error(&error);
                        attempt = attempt.saturating_add(1);
                        if matches!(self.wait_retry(attempt).await, LoopControl::Stop) {
                            return;
                        }
                        continue;
                    }
                }
            }

            match self.run_ipc_session(connection).await {
                SessionExit::Stop => return,
                SessionExit::Disconnected {
                    error,
                    synchronized,
                } => {
                    // 主体随连接消亡：下一条连接要重新 bootstrap 才有。
                    self.state.principal = None;
                    self.set_error(&error);
                    if synchronized {
                        attempt = 0;
                    }
                    attempt = attempt.saturating_add(1);
                    if matches!(self.wait_retry(attempt).await, LoopControl::Stop) {
                        return;
                    }
                }
            }
        }
    }

    async fn run_ipc_session(&mut self, connection: StreamConnection) -> SessionExit {
        let (mut reader, mut writer) = connection.into_split();
        let mut pending = BTreeMap::new();
        let mut snapshot = None;
        let mut synchronized = false;
        let mut immediate_refreshes = 0_usize;
        if let Err(error) = self
            .start_snapshot(&mut writer, &mut pending, &mut snapshot, None, Vec::new())
            .await
        {
            if self.shutdown_requested() {
                return SessionExit::Stop;
            }
            return SessionExit::Disconnected {
                error,
                synchronized,
            };
        }
        let tick_period = self.options.request_timeout.min(Duration::from_millis(100));
        let mut deadline_tick = tokio::time::interval(tick_period);

        loop {
            let mut shutdown = self.shutdown.clone();
            tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => {
                    let error = ClientError::connection("连接 actor 正在关闭");
                    fail_pending(&mut pending, &mut snapshot, &error);
                    return SessionExit::Stop;
                }
                incoming = reader.next_incoming() => {
                    let incoming = match incoming {
                        Ok(incoming) => incoming,
                        Err(error) => {
                            fail_pending(&mut pending, &mut snapshot, &error);
                            return SessionExit::Disconnected { error, synchronized };
                        }
                    };
                    let follow_up_waiters = match self.handle_incoming(
                        incoming,
                        &mut pending,
                        &mut snapshot,
                        &mut synchronized,
                        immediate_refreshes < MAX_IMMEDIATE_REFRESHES,
                    ) {
                        Ok(follow_up_waiters) => follow_up_waiters,
                        Err(error) => {
                            fail_pending(&mut pending, &mut snapshot, &error);
                            return SessionExit::Disconnected { error, synchronized };
                        }
                    };
                    if let Some(waiters) = follow_up_waiters {
                        immediate_refreshes = immediate_refreshes.saturating_add(1);
                        if let Err(error) = self.start_snapshot(
                            &mut writer,
                            &mut pending,
                            &mut snapshot,
                            None,
                            waiters,
                        ).await {
                            if self.shutdown_requested() {
                                fail_pending(&mut pending, &mut snapshot, &error);
                                return SessionExit::Stop;
                            }
                            fail_pending(&mut pending, &mut snapshot, &error);
                            return SessionExit::Disconnected { error, synchronized };
                        }
                    } else if snapshot.is_none() {
                        immediate_refreshes = 0;
                    }
                }
                command = self.commands.recv() => {
                    match command {
                        Some(Command::Request { method, params, deadline, reply }) => {
                            if reply.is_closed() || deadline <= Instant::now() {
                                let _ = reply.send(Err(ClientError::connection("JSON-RPC 请求超时")));
                                continue;
                            }
                            if pending.len() >= MAX_PENDING_REQUESTS {
                                let _ = reply.send(Err(ClientError::connection("pending 请求已达上限")));
                                continue;
                            }
                            let mut shutdown = self.shutdown.clone();
                            let send = await_or_shutdown(
                                &mut shutdown,
                                tokio::time::timeout_at(deadline, writer.send_request(&method, params)),
                            ).await;
                            let Some(send) = send else {
                                let error = ClientError::connection("连接 actor 正在关闭");
                                let _ = reply.send(Err(error.clone()));
                                fail_pending(&mut pending, &mut snapshot, &error);
                                return SessionExit::Stop;
                            };
                            let send = send
                                .map_err(|_| ClientError::connection("JSON-RPC 请求超时"))
                                .and_then(|result| result);
                            match send {
                                Ok(id) => {
                                    let Some(id) = id.as_u64() else {
                                        let error = ClientError::rpc("客户端生成了无效请求 id");
                                        let _ = reply.send(Err(error.clone()));
                                        fail_pending(&mut pending, &mut snapshot, &error);
                                        return SessionExit::Disconnected { error, synchronized };
                                    };
                                    pending.insert(id, PendingRequest {
                                        deadline,
                                        kind: PendingKind::User(reply),
                                    });
                                }
                                Err(error) if error.kind == ErrorKind::Rpc => {
                                    let _ = reply.send(Err(error));
                                }
                                Err(error) => {
                                    let _ = reply.send(Err(error.clone()));
                                    fail_pending(&mut pending, &mut snapshot, &error);
                                    return SessionExit::Disconnected { error, synchronized };
                                }
                            }
                        }
                        Some(Command::Refresh { deadline, reply }) => {
                            if reply.is_closed() || deadline <= Instant::now() {
                                let _ = reply.send(Err(ClientError::connection("刷新请求超时")));
                                continue;
                            }
                            immediate_refreshes = 0;
                            let result = self.start_snapshot(
                                &mut writer,
                                &mut pending,
                                &mut snapshot,
                                Some(deadline),
                                vec![reply],
                            ).await;
                            if let Err(error) = result {
                                if self.shutdown_requested() {
                                    fail_pending(&mut pending, &mut snapshot, &error);
                                    return SessionExit::Stop;
                                }
                                fail_pending(&mut pending, &mut snapshot, &error);
                                return SessionExit::Disconnected { error, synchronized };
                            }
                        }
                        None => {
                            let error = ClientError::connection("所有连接 handle 已释放");
                            fail_pending(&mut pending, &mut snapshot, &error);
                            return SessionExit::Stop;
                        }
                    }
                }
                _ = deadline_tick.tick() => {
                    let timed_out = pending
                        .iter()
                        .filter_map(|(id, request)| (request.deadline <= Instant::now()).then_some(*id))
                        .collect::<Vec<_>>();
                    for id in timed_out {
                        if let Some(request) = pending.remove(&id) {
                            let error = ClientError::connection("JSON-RPC 请求超时");
                            match request.kind {
                                PendingKind::User(reply) => { let _ = reply.send(Err(error)); }
                                PendingKind::Snapshot(part) => {
                                    if let Some(snapshot) = snapshot.as_mut() {
                                        snapshot.record(part, Err(error));
                                    }
                                }
                            }
                        }
                    }
                    if snapshot.as_ref().is_some_and(SnapshotTracker::is_complete) {
                        let outcome = match self.finish_snapshot(
                            &mut snapshot,
                            immediate_refreshes < MAX_IMMEDIATE_REFRESHES,
                            !synchronized,
                        ) {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                fail_pending(&mut pending, &mut snapshot, &error);
                                return SessionExit::Disconnected { error, synchronized };
                            }
                        };
                        if outcome.follow_up {
                            immediate_refreshes = immediate_refreshes.saturating_add(1);
                            if let Err(error) = self.start_snapshot(
                                &mut writer,
                                &mut pending,
                                &mut snapshot,
                                None,
                                outcome.waiters,
                            ).await {
                                if self.shutdown_requested() {
                                    fail_pending(&mut pending, &mut snapshot, &error);
                                    return SessionExit::Stop;
                                }
                                fail_pending(&mut pending, &mut snapshot, &error);
                                return SessionExit::Disconnected { error, synchronized };
                            }
                        } else {
                            immediate_refreshes = 0;
                        }
                        synchronized = true;
                    }
                }
            }
        }
    }

    async fn start_snapshot(
        &mut self,
        writer: &mut StreamWriter,
        pending: &mut BTreeMap<u64, PendingRequest>,
        snapshot: &mut Option<SnapshotTracker>,
        deadline: Option<Instant>,
        mut waiters: Vec<RefreshWaiter>,
    ) -> Result<(), ClientError> {
        if let Some(snapshot) = snapshot.as_mut() {
            for waiter in waiters {
                if snapshot.waiters.len() >= MAX_REFRESH_WAITERS {
                    let _ = waiter.send(Err(ClientError::connection("等待中的刷新请求已达上限")));
                } else {
                    snapshot.waiters.push(waiter);
                }
            }
            return Ok(());
        }
        let deadline = deadline.unwrap_or_else(|| Instant::now() + self.options.request_timeout);
        if deadline <= Instant::now() {
            let error = ClientError::connection("刷新请求超时");
            if !waiters.is_empty() {
                fail_refresh_waiters(&mut waiters, &error);
                return Ok(());
            }
            return Err(error);
        }
        if pending.len().saturating_add(SNAPSHOT_REQUESTS) > MAX_PENDING_REQUESTS {
            let error = ClientError::connection("pending 请求不足以容纳刷新快照");
            if !waiters.is_empty() {
                fail_refresh_waiters(&mut waiters, &error);
                return Ok(());
            }
            return Err(error);
        }
        self.set_phase(ConnectionPhase::Synchronizing);
        let mut tracker = SnapshotTracker::new();
        tracker.waiters = waiters;
        *snapshot = Some(tracker);
        let requests = [
            (SnapshotPart::Status, "status", json!({})),
            (SnapshotPart::Accounts, "listAccounts", json!({})),
            (
                SnapshotPart::Messages,
                "listMessageSummaries",
                json!({ "limit": self.options.initial_page_size, "offset": 0 }),
            ),
            // ADR-0048：每次（重）连接与刷新都重读偏好——推送是尽力而为，`system` 模式下系统语言
            // 变化也只在重读时体现。
            (SnapshotPart::Locale, "getLocalePreference", json!({})),
        ];
        for (part, method, params) in requests {
            let mut shutdown = self.shutdown.clone();
            let id = await_or_shutdown(
                &mut shutdown,
                tokio::time::timeout_at(deadline, writer.send_request(method, params)),
            )
            .await
            .ok_or_else(|| ClientError::connection("连接 actor 正在关闭"))?
            .map_err(|_| ClientError::connection("快照请求超时"))??;
            let id = id
                .as_u64()
                .ok_or_else(|| ClientError::rpc("客户端生成了无效快照请求 id"))?;
            pending.insert(
                id,
                PendingRequest {
                    deadline,
                    kind: PendingKind::Snapshot(part),
                },
            );
        }
        Ok(())
    }

    fn handle_incoming(
        &mut self,
        incoming: Incoming,
        pending: &mut BTreeMap<u64, PendingRequest>,
        snapshot: &mut Option<SnapshotTracker>,
        synchronized: &mut bool,
        allow_follow_up: bool,
    ) -> Result<Option<Vec<RefreshWaiter>>, ClientError> {
        match incoming {
            // ADR-0021 反向请求只面向注册了 provider 的插件连接；
            // 桌面连接不是 provider，正常流里不会出现。出现即按协议错误处理。
            Incoming::Request { id, .. } => Err(ClientError::rpc(format!(
                "收到意外的反向请求 id={id}（本连接不是 transport provider）"
            ))),
            Incoming::Notification(notification) => {
                let notification = notification.into_server_notification()?;
                validate_notification(&notification)?;
                if matches!(notification, ServerNotification::Unknown(_)) {
                    return Ok(None);
                }
                // ADR-0045：意图唤醒不是快照状态——不进同步缓冲（否则快照收尾会重放、重复计数），
                // 同步期间也立即生效。只认发给本连接所声明前端的那一条。
                if let ServerNotification::FrontendIntent { frontend } = &notification {
                    if self
                        .options
                        .identity
                        .as_ref()
                        .is_some_and(|identity| identity.frontend_id == *frontend)
                    {
                        self.state.intent_epoch = self.state.intent_epoch.wrapping_add(1);
                        self.publish();
                        let _ = self.events.send(AppEvent::Notification(notification));
                    }
                    return Ok(None);
                }
                // ADR-0046：停机信号同样不是快照状态，不进缓冲、不重放；只在首次收到时发布一次。
                if matches!(notification, ServerNotification::AgentStopping) {
                    if !self.state.shutdown_requested {
                        self.state.shutdown_requested = true;
                        self.publish();
                        let _ = self.events.send(AppEvent::Notification(notification));
                    }
                    return Ok(None);
                }
                // ADR-0048：语言偏好即时生效，不进缓冲；同步期间收到时它比快照里的应答新。
                if let ServerNotification::LocalePreference { effective } = &notification {
                    if let Some(snapshot) = snapshot.as_mut() {
                        snapshot.locale_pushed = true;
                    }
                    if self.state.locale.as_ref() != Some(effective) {
                        self.state.locale = Some(effective.clone());
                        self.publish();
                    }
                    let _ = self.events.send(AppEvent::Notification(notification));
                    return Ok(None);
                }
                let synchronizing = snapshot.is_some();
                if let Some(snapshot) = snapshot.as_mut() {
                    snapshot.buffer(notification.clone());
                }
                if apply_notification(
                    &mut self.state,
                    &notification,
                    self.options.initial_page_size,
                    true,
                ) {
                    self.publish();
                    let _ = self.events.send(AppEvent::Notification(notification));
                } else if synchronizing {
                    let _ = self.events.send(AppEvent::Notification(notification));
                }
                Ok(None)
            }
            Incoming::Response { id, result } => {
                let id = id
                    .as_u64()
                    .ok_or_else(|| ClientError::rpc("服务端响应 id 必须是无符号整数"))?;
                let Some(request) = pending.remove(&id) else {
                    return Ok(None);
                };
                let result = result.map_err(rpc_error);
                match request.kind {
                    PendingKind::User(reply) => {
                        let _ = reply.send(result);
                    }
                    PendingKind::Snapshot(part) => {
                        if let Some(snapshot) = snapshot.as_mut() {
                            snapshot.record(part, result);
                        }
                        if snapshot.as_ref().is_some_and(SnapshotTracker::is_complete) {
                            let outcome =
                                self.finish_snapshot(snapshot, allow_follow_up, !*synchronized)?;
                            *synchronized = true;
                            return Ok(outcome.follow_up.then_some(outcome.waiters));
                        }
                    }
                }
                Ok(None)
            }
        }
    }

    fn finish_snapshot(
        &mut self,
        snapshot: &mut Option<SnapshotTracker>,
        allow_follow_up: bool,
        new_connection: bool,
    ) -> Result<SnapshotOutcome, ClientError> {
        let mut snapshot = snapshot
            .take()
            .ok_or_else(|| ClientError::rpc("内部快照状态缺失"))?;
        let locale = snapshot
            .locale
            .take()
            .expect("complete snapshot has locale");
        if !snapshot.locale_pushed {
            apply_locale_result(&mut self.state, locale);
        }
        let status_value = match snapshot
            .status
            .take()
            .expect("complete snapshot has status")
        {
            Ok(value) => value,
            Err(error) => {
                fail_refresh_waiters(&mut snapshot.waiters, &error);
                return Err(error);
            }
        };
        let status: StatusResult = match decode("status", status_value) {
            Ok(status) => status,
            Err(error) => {
                fail_refresh_waiters(&mut snapshot.waiters, &error);
                return Err(error);
            }
        };
        if let Err(error) = validate_status(&status) {
            fail_refresh_waiters(&mut snapshot.waiters, &error);
            return Err(error);
        }
        let mut partial_errors = Vec::new();

        let accounts = match snapshot
            .accounts
            .take()
            .expect("complete snapshot has accounts")
        {
            Ok(value) => match decode::<AccountsResult>("listAccounts", value) {
                Ok(result) => match validate_accounts(&result.accounts) {
                    Ok(()) => result.accounts,
                    Err(error) => {
                        partial_errors.push(error.to_string());
                        status.accounts.clone()
                    }
                },
                Err(error) => {
                    partial_errors.push(error.to_string());
                    status.accounts.clone()
                }
            },
            Err(error) => {
                partial_errors.push(error.to_string());
                status.accounts.clone()
            }
        };
        let messages = match snapshot
            .messages
            .take()
            .expect("complete snapshot has messages")
        {
            Ok(value) => match decode::<MessageSummariesResult>("listMessageSummaries", value) {
                Ok(result) => match validate_messages(&result, self.options.initial_page_size) {
                    Ok(()) => Some(result),
                    Err(error) => {
                        partial_errors.push(error.to_string());
                        None
                    }
                },
                Err(error) => {
                    partial_errors.push(error.to_string());
                    None
                }
            },
            Err(error) => {
                partial_errors.push(error.to_string());
                None
            }
        };

        self.state.version = Some(status.version);
        self.state.accounts = accounts;
        if let Some(messages) = messages {
            self.state.messages = messages.messages;
            self.state.total_messages = messages.total;
        } else {
            self.state.total_messages = status.message_count;
        }
        let mut dropped_unknown_account = false;
        let mut missing_buffered_message = false;
        for notification in snapshot.buffered {
            if let ServerNotification::MessageReceived(message) = &notification {
                missing_buffered_message |= !self.state.messages.iter().any(|candidate| {
                    candidate.account == message.account && candidate.uid == message.uid
                });
            }
            if !apply_notification(
                &mut self.state,
                &notification,
                self.options.initial_page_size,
                false,
            ) {
                dropped_unknown_account = true;
            }
        }
        let refresh_requested = snapshot.overflowed || missing_buffered_message;
        let follow_up = refresh_requested && allow_follow_up;
        if snapshot.overflowed {
            partial_errors.push("通知缓冲达到上限，正在重新同步".into());
        } else if missing_buffered_message {
            partial_errors.push("快照页外收到新消息，正在确认最新总数".into());
        }
        if dropped_unknown_account {
            partial_errors.push("已丢弃未知账户通知".into());
        }
        if refresh_requested && !allow_follow_up {
            partial_errors.push("连续自动重新同步已达上限，请稍后手动刷新".into());
        }
        self.state.partial_snapshot = !partial_errors.is_empty();
        self.state.last_error = (!partial_errors.is_empty()).then(|| partial_errors.join("; "));
        self.state.phase = if follow_up {
            ConnectionPhase::Synchronizing
        } else {
            ConnectionPhase::Online
        };
        if new_connection {
            self.state.connection_epoch = self.state.connection_epoch.saturating_add(1);
        }
        self.publish();
        let mut waiters = snapshot.waiters;
        if !follow_up {
            if refresh_requested {
                fail_refresh_waiters(
                    &mut waiters,
                    &ClientError::rpc("连续通知导致快照无法稳定，请稍后重试"),
                );
            } else {
                for waiter in std::mem::take(&mut waiters) {
                    let _ = waiter.send(Ok(()));
                }
            }
        }
        Ok(SnapshotOutcome { follow_up, waiters })
    }

    async fn run_http(&mut self) {
        let mut attempt = 0_u32;
        loop {
            self.set_phase(ConnectionPhase::Connecting { attempt });
            self.set_phase(ConnectionPhase::Synchronizing);
            let deadline = Instant::now() + self.options.request_timeout;
            let mut shutdown = self.shutdown.clone();
            let Some(reconcile) =
                await_or_shutdown(&mut shutdown, self.reconcile_http(deadline, true)).await
            else {
                return;
            };
            if let Err(error) = reconcile {
                self.set_error(&error);
                attempt = attempt.saturating_add(1);
                if matches!(self.wait_retry(attempt).await, LoopControl::Stop) {
                    return;
                }
                continue;
            }
            attempt = 0;
            let poll = tokio::time::sleep(self.options.http_poll_interval);
            tokio::pin!(poll);
            loop {
                let mut shutdown = self.shutdown.clone();
                tokio::select! {
                    biased;
                    _ = wait_for_shutdown(&mut shutdown) => return,
                    _ = &mut poll => {
                        self.set_phase(ConnectionPhase::Synchronizing);
                        let deadline = Instant::now() + self.options.request_timeout;
                        let mut shutdown = self.shutdown.clone();
                        let Some(reconcile) = await_or_shutdown(
                            &mut shutdown,
                            self.reconcile_http(deadline, false),
                        ).await else {
                            return;
                        };
                        match reconcile {
                            Ok(()) => poll.as_mut().reset(Instant::now() + self.options.http_poll_interval),
                            Err(error) => {
                                self.set_error(&error);
                                attempt = attempt.saturating_add(1);
                                break;
                            }
                        }
                    }
                    command = self.commands.recv() => {
                        match command {
                            Some(Command::Request { method, params, deadline, reply }) => {
                                if reply.is_closed() || deadline <= Instant::now() {
                                    let _ = reply.send(Err(ClientError::connection("JSON-RPC 请求超时")));
                                    continue;
                                }
                                let mut shutdown = self.shutdown.clone();
                                let Some(result) = await_or_shutdown(
                                    &mut shutdown,
                                    self.http_request_at(&method, params, deadline),
                                ).await else {
                                    let _ = reply.send(Err(ClientError::connection("连接 actor 正在关闭")));
                                    return;
                                };
                                let disconnected = result.as_ref().is_err_and(|error| error.kind == ErrorKind::Connection);
                                let _ = reply.send(result.clone());
                                if disconnected {
                                    self.set_error(&result.expect_err("checked error"));
                                    attempt = attempt.saturating_add(1);
                                    break;
                                }
                            }
                            Some(Command::Refresh { deadline, reply }) => {
                                if reply.is_closed() || deadline <= Instant::now() {
                                    let _ = reply.send(Err(ClientError::connection("刷新请求超时")));
                                    continue;
                                }
                                self.set_phase(ConnectionPhase::Synchronizing);
                                let mut shutdown = self.shutdown.clone();
                                let Some(result) = await_or_shutdown(
                                    &mut shutdown,
                                    self.reconcile_http(deadline, false),
                                ).await else {
                                    let _ = reply.send(Err(ClientError::connection("连接 actor 正在关闭")));
                                    return;
                                };
                                let _ = reply.send(result.clone());
                                if let Err(error) = result {
                                    self.set_error(&error);
                                    attempt = attempt.saturating_add(1);
                                    break;
                                }
                                poll.as_mut().reset(Instant::now() + self.options.http_poll_interval);
                            }
                            None => return,
                        }
                    }
                }
            }
            if matches!(self.wait_retry(attempt).await, LoopControl::Stop) {
                return;
            }
        }
    }

    async fn reconcile_http(
        &mut self,
        deadline: Instant,
        new_connection: bool,
    ) -> Result<(), ClientError> {
        let status: StatusResult = self
            .http_request_typed_at("status", json!({}), deadline)
            .await?;
        validate_status(&status)?;
        let accounts = self
            .http_request_typed_at::<AccountsResult>("listAccounts", json!({}), deadline)
            .await;
        let messages = self
            .http_request_typed_at::<MessageSummariesResult>(
                "listMessageSummaries",
                json!({ "limit": self.options.initial_page_size, "offset": 0 }),
                deadline,
            )
            .await;
        // ADR-0048 的语言偏好只在 IPC 快照里取：HTTP 会话恒为 user 主体、不承载推送，
        // 以前端身份运行的壳（maild-ui）不会走到这里，`locale` 保持 `None`。
        if let Err(error) = &accounts {
            if error.kind == ErrorKind::Connection {
                return Err(error.clone());
            }
        }
        if let Err(error) = &messages {
            if error.kind == ErrorKind::Connection {
                return Err(error.clone());
            }
        }
        let mut partial_errors = Vec::new();
        self.state.version = Some(status.version);
        self.state.accounts = match accounts {
            Ok(accounts) => match validate_accounts(&accounts.accounts) {
                Ok(()) => accounts.accounts,
                Err(error) => {
                    partial_errors.push(error.to_string());
                    status.accounts
                }
            },
            Err(error) => {
                partial_errors.push(error.to_string());
                status.accounts
            }
        };
        match messages {
            Ok(messages) => match validate_messages(&messages, self.options.initial_page_size) {
                Ok(()) => {
                    self.state.messages = messages.messages;
                    self.state.total_messages = messages.total;
                }
                Err(error) => {
                    partial_errors.push(error.to_string());
                    self.state.total_messages = status.message_count;
                }
            },
            Err(error) => {
                partial_errors.push(error.to_string());
                self.state.total_messages = status.message_count;
            }
        }
        self.state.partial_snapshot = !partial_errors.is_empty();
        self.state.last_error = (!partial_errors.is_empty()).then(|| partial_errors.join("; "));
        self.state.phase = ConnectionPhase::Online;
        if new_connection {
            self.state.connection_epoch = self.state.connection_epoch.saturating_add(1);
        }
        self.publish();
        Ok(())
    }

    async fn http_request_at(
        &self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, ClientError> {
        tokio::time::timeout_at(deadline, self.client.call(method, params))
            .await
            .map_err(|_| ClientError::connection("HTTP JSON-RPC 请求超时"))?
    }

    async fn http_request_typed_at<T>(
        &self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<T, ClientError>
    where
        T: serde::de::DeserializeOwned,
    {
        decode(
            method,
            self.http_request_at(method, params, deadline).await?,
        )
    }

    async fn wait_retry(&mut self, attempt: u32) -> LoopControl {
        let delay = self.options.reconnect.delay(attempt);
        self.set_phase(ConnectionPhase::RetryWaiting { attempt, delay });
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            let mut shutdown = self.shutdown.clone();
            tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => return LoopControl::Stop,
                _ = &mut sleep => return LoopControl::Continue,
                command = self.commands.recv() => {
                    if matches!(self.handle_offline_command(command, "连接正在等待重试").await, LoopControl::Stop) {
                        return LoopControl::Stop;
                    }
                }
            }
        }
    }

    async fn handle_offline_command(
        &mut self,
        command: Option<Command>,
        detail: &str,
    ) -> LoopControl {
        let error = ClientError::connection(detail);
        match command {
            Some(Command::Request { reply, .. }) => {
                let _ = reply.send(Err(error));
                LoopControl::Continue
            }
            Some(Command::Refresh { reply, .. }) => {
                let _ = reply.send(Err(error));
                LoopControl::Continue
            }
            None => LoopControl::Stop,
        }
    }
}

fn decode<T>(method: &str, value: Value) -> Result<T, ClientError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(value)
        .map_err(|error| ClientError::rpc(format!("{method} 响应结构无效: {error}")))
}

fn rpc_error(error: JsonRpcError) -> ClientError {
    ClientError {
        kind: ErrorKind::Rpc,
        message: error.message,
        rpc_code: Some(error.code),
        rpc_data: error.data,
    }
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

async fn await_or_shutdown<T>(
    shutdown: &mut watch::Receiver<bool>,
    future: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = wait_for_shutdown(shutdown) => None,
        result = future => Some(result),
    }
}

fn fail_refresh_waiters(
    waiters: &mut Vec<oneshot::Sender<Result<(), ClientError>>>,
    error: &ClientError,
) {
    for waiter in std::mem::take(waiters) {
        let _ = waiter.send(Err(error.clone()));
    }
}

fn fail_pending(
    pending: &mut BTreeMap<u64, PendingRequest>,
    snapshot: &mut Option<SnapshotTracker>,
    error: &ClientError,
) {
    for (_, request) in std::mem::take(pending) {
        if let PendingKind::User(reply) = request.kind {
            let _ = reply.send(Err(error.clone()));
        }
    }
    if let Some(snapshot) = snapshot.as_mut() {
        fail_refresh_waiters(&mut snapshot.waiters, error);
    }
}

fn notification_bytes(notification: &ServerNotification) -> usize {
    const OVERHEAD: usize = 128;
    match notification {
        ServerNotification::AccountState(update) => OVERHEAD
            .saturating_add(update.account.len())
            .saturating_add(update.state.len())
            .saturating_add(update.detail.len())
            .saturating_add(update.detail_code.as_deref().map_or(0, str::len))
            .saturating_add(update.server_detail.len()),
        ServerNotification::MessageReceived(message) => OVERHEAD
            .saturating_add(message.account.len())
            .saturating_add(message.uid.len())
            .saturating_add(message.sender.len())
            .saturating_add(message.subject.len())
            .saturating_add(message.body.len()),
        ServerNotification::FrontendIntent { frontend } => OVERHEAD.saturating_add(frontend.len()),
        ServerNotification::AgentStopping => OVERHEAD,
        ServerNotification::LocalePreference { effective } => effective
            .iter()
            .fold(OVERHEAD, |total, tag| total.saturating_add(tag.len())),
        ServerNotification::Unknown(value) => OVERHEAD
            .saturating_add(value.method.len())
            .saturating_add(value.params.to_string().len()),
    }
}

fn validate_text(label: &str, value: &str, max_bytes: usize) -> Result<(), ClientError> {
    if value.len() > max_bytes {
        return Err(ClientError::rpc(format!(
            "{label} 超过 {max_bytes} bytes 的通知上限"
        )));
    }
    Ok(())
}

fn validate_accounts(accounts: &[crate::AccountState]) -> Result<(), ClientError> {
    if accounts.len() > MAX_ACCOUNTS {
        return Err(ClientError::rpc(format!(
            "账户快照超过 {MAX_ACCOUNTS} 条上限"
        )));
    }
    for account in accounts {
        validate_text("账户名", &account.account, MAX_NOTIFICATION_TEXT_BYTES)?;
        validate_text("账户状态", &account.state, MAX_NOTIFICATION_TEXT_BYTES)?;
        validate_text("账户详情", &account.detail, MAX_NOTIFICATION_TEXT_BYTES)?;
        if let Some(code) = &account.detail_code {
            validate_text("账户详情 code", code, MAX_NOTIFICATION_TEXT_BYTES)?;
        }
        validate_text("账户服务器原文", &account.server_detail, MAX_NOTIFICATION_TEXT_BYTES)?;
    }
    Ok(())
}

fn validate_status(status: &crate::StatusResult) -> Result<(), ClientError> {
    validate_text("服务端版本", &status.version, MAX_NOTIFICATION_TEXT_BYTES)?;
    if status.message_count < 0 {
        return Err(ClientError::rpc("status message_count 不能为负数"));
    }
    validate_accounts(&status.accounts)
}

fn validate_messages(
    messages: &crate::MessageSummariesResult,
    limit: u64,
) -> Result<(), ClientError> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if messages.messages.len() > limit {
        return Err(ClientError::rpc(format!(
            "邮件快照超过请求的 {limit} 条上限"
        )));
    }
    if messages.total < 0 {
        return Err(ClientError::rpc("邮件快照 total 不能为负数"));
    }
    if messages.total < messages.messages.len() as i64 {
        return Err(ClientError::rpc("邮件快照 total 不能小于当前页条数"));
    }
    for message in &messages.messages {
        validate_text("邮件账户", &message.account, MAX_NOTIFICATION_TEXT_BYTES)?;
        validate_text("邮件 UID", &message.uid, MAX_NOTIFICATION_TEXT_BYTES)?;
        validate_text("邮件发件人", &message.sender, MAX_NOTIFICATION_TEXT_BYTES)?;
        validate_text("邮件主题", &message.subject, MAX_NOTIFICATION_TEXT_BYTES)?;
    }
    Ok(())
}

fn validate_notification(notification: &ServerNotification) -> Result<(), ClientError> {
    match notification {
        ServerNotification::AccountState(update) => {
            validate_text(
                "account/state account",
                &update.account,
                MAX_NOTIFICATION_TEXT_BYTES,
            )?;
            validate_text(
                "account/state state",
                &update.state,
                MAX_NOTIFICATION_TEXT_BYTES,
            )?;
            validate_text(
                "account/state detail",
                &update.detail,
                MAX_NOTIFICATION_TEXT_BYTES,
            )?;
            if let Some(code) = &update.detail_code {
                validate_text("account/state detail_code", code, MAX_NOTIFICATION_TEXT_BYTES)?;
            }
            validate_text(
                "account/state server_detail",
                &update.server_detail,
                MAX_NOTIFICATION_TEXT_BYTES,
            )
        }
        ServerNotification::MessageReceived(message) => {
            validate_text(
                "message/received account",
                &message.account,
                MAX_NOTIFICATION_TEXT_BYTES,
            )?;
            validate_text(
                "message/received uid",
                &message.uid,
                MAX_NOTIFICATION_TEXT_BYTES,
            )?;
            validate_text(
                "message/received sender",
                &message.sender,
                MAX_NOTIFICATION_TEXT_BYTES,
            )?;
            validate_text(
                "message/received subject",
                &message.subject,
                MAX_NOTIFICATION_TEXT_BYTES,
            )?;
            validate_text(
                "message/received body",
                &message.body,
                MAX_NOTIFICATION_BODY_BYTES,
            )
        }
        ServerNotification::FrontendIntent { frontend } => validate_text(
            "frontend/intent frontend",
            frontend,
            MAX_NOTIFICATION_TEXT_BYTES,
        ),
        ServerNotification::LocalePreference { effective } => validate_locale(effective),
        ServerNotification::AgentStopping | ServerNotification::Unknown(_) => Ok(()),
    }
}

/// `effective` 的形状上限（ADR-0048 决策 2：永不为空）。内容是否合法 BCP 47 由前端协商时自行判断，
/// 认不出的标签只是匹配不上。
fn validate_locale(effective: &[String]) -> Result<(), ClientError> {
    if effective.is_empty() {
        return Err(ClientError::rpc("语言偏好 effective 不能为空"));
    }
    if effective.len() > MAX_LOCALE_TAGS {
        return Err(ClientError::rpc(format!(
            "语言偏好 effective 超过 {MAX_LOCALE_TAGS} 项上限"
        )));
    }
    for tag in effective {
        validate_text("语言标签", tag, MAX_LOCALE_TAG_BYTES)?;
    }
    Ok(())
}

/// `getLocalePreference` 应答（ADR-0048）。只取 `effective`：偏好模式由设置页按需另取。
#[derive(serde::Deserialize)]
struct LocaleSnapshot {
    effective: Vec<String>,
}

/// 把一次 `getLocalePreference` 的结果落进状态。
///
/// - 成功：更新为新的 `effective`。
/// - `-32601`（老 daemon 没有这个方法）或 `-32016`（老 daemon 的授权表不认识它，对前端主体
///   fail-closed）：daemon 不提供偏好，清空为 `None`，前端回落基础语言。
/// - 其他失败（超时、应答结构不对）：保留上一次的值，不让一次抖动把界面语言打回默认；也不记进
///   `partial_snapshot`——语言偏好不是邮箱数据。
fn apply_locale_result(state: &mut ViewState, result: Result<Value, ClientError>) {
    match result {
        Ok(value) => {
            if let Ok(effective) = decode::<LocaleSnapshot>("getLocalePreference", value)
                .and_then(|locale| validate_locale(&locale.effective).map(|()| locale.effective))
            {
                state.locale = Some(effective);
            }
        }
        Err(error) if matches!(error.rpc_code, Some(-32601 | -32016)) => state.locale = None,
        Err(_) => {}
    }
}

/// ADR-0030 决策 6 的三步：短命 user 连接取回并轮换 token → 长连接 authenticate → whoami 校验。
///
/// 第一步经 `RpcClient::call`：IPC 下它每请求开一条新连接、应答即关。那条连接是 user 主体，
/// 这一点无法消除——也正是 ADR-0030 说「这条上限不可强制」的原因。token 只活在本函数栈上。
async fn bind_identity(
    client: &RpcClient,
    connection: &mut StreamConnection,
    identity: &crate::FrontendIdentity,
) -> Result<String, (IdentityFailure, ClientError)> {
    let token = client
        .reveal_plugin_token(&identity.frontend_id, Some("frontend"))
        .await
        .map_err(|error| match error.kind {
            // 参数错误 = 包不在册或没有身份条目：修复动作是安装，不是等待。
            ErrorKind::Rpc => (
                IdentityFailure::NotInstalled,
                ClientError::rpc(format!(
                    "前端包 `{}` 未安装或没有身份条目（{}）",
                    identity.frontend_id, error.message
                )),
            ),
            ErrorKind::Connection => (IdentityFailure::Unavailable, error),
        })?;
    connection.authenticate(&token).await.map_err(|error| {
        (
            IdentityFailure::Rejected,
            ClientError::rpc(format!("前端 `{}` 认证被拒绝：{}", identity.frontend_id, error.message)),
        )
    })?;
    drop(token);
    let whoami = connection
        .call("whoami", json!({}), |_| {})
        .await
        .map_err(|error| match error.kind {
            ErrorKind::Connection => (IdentityFailure::Unavailable, error),
            ErrorKind::Rpc => (IdentityFailure::Rejected, error),
        })?;
    let principal = whoami["principal"].as_str().ok_or_else(|| {
        (
            IdentityFailure::Rejected,
            ClientError::rpc("whoami 应答缺少 principal"),
        )
    })?;
    let expected = identity.expected_principal();
    if principal != expected {
        return Err((
            IdentityFailure::PrincipalMismatch,
            ClientError::rpc(format!("会话主体是 {principal}，不是预期的 {expected}")),
        ));
    }
    Ok(principal.to_owned())
}

fn apply_notification(
    state: &mut ViewState,
    notification: &ServerNotification,
    limit: u64,
    count_new: bool,
) -> bool {
    match notification {
        ServerNotification::AccountState(update) => {
            if let Some(account) = state
                .accounts
                .iter_mut()
                .find(|account| account.account == update.account)
            {
                *account = update.clone();
                true
            } else {
                false
            }
        }
        ServerNotification::MessageReceived(message) => {
            if !state
                .accounts
                .iter()
                .any(|account| account.account == message.account)
            {
                return false;
            }
            let existing = state.messages.iter().position(|candidate| {
                candidate.account == message.account && candidate.uid == message.uid
            });
            if let Some(index) = existing {
                state.messages.remove(index);
            } else if count_new {
                state.total_messages = state.total_messages.saturating_add(1);
            }
            state.messages.push(MessageSummary::from(message.as_ref()));
            state.messages.sort_by(|left, right| {
                right
                    .sort_at
                    .cmp(&left.sort_at)
                    .then_with(|| right.uid.cmp(&left.uid))
            });
            state.messages.truncate(limit as usize);
            true
        }
        // 由 `handle_incoming` 直接处理，从不进快照缓冲。
        ServerNotification::FrontendIntent { .. }
        | ServerNotification::AgentStopping
        | ServerNotification::LocalePreference { .. }
        | ServerNotification::Unknown(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConnectionHandle, MailMessage, ReconnectPolicy, TransportKind};
    use maild_client::Endpoint;
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

    fn test_options() -> ActorOptions {
        ActorOptions {
            initial_page_size: 10,
            request_timeout: Duration::from_millis(100),
            http_poll_interval: Duration::from_secs(1),
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(100),
                max_delay: Duration::from_millis(200),
            },
            identity: None,
        }
    }

    fn test_actor() -> (
        Actor,
        mpsc::Sender<Command>,
        watch::Receiver<ViewState>,
        broadcast::Receiver<AppEvent>,
        watch::Sender<bool>,
    ) {
        let client = RpcClient::new(Endpoint::Http {
            url: "http://127.0.0.1:9/rpc".into(),
            token: "t".repeat(32),
        })
        .unwrap();
        let (commands_tx, commands_rx) = mpsc::channel(16);
        let state = ViewState::new(TransportKind::Unix);
        let (state_tx, state_rx) = watch::channel(state.clone());
        let (events_tx, events_rx) = broadcast::channel(16);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        (
            Actor {
                client: Arc::new(client),
                options: test_options(),
                commands: commands_rx,
                state_tx,
                events: events_tx,
                shutdown: shutdown_rx,
                state,
            },
            commands_tx,
            state_rx,
            events_rx,
            shutdown_tx,
        )
    }

    /// 假 daemon：只认 ADR-0030 决策 6 的握手。每条连接的首个方法都记进日志，
    /// 让测试能断言「reveal 走的是另一条短命连接」「长连接首帧是 authenticate」。
    #[cfg(unix)]
    mod identity_server {
        use super::*;
        use std::sync::Mutex;
        use tokio::net::UnixListener;

        #[derive(Clone, Copy, PartialEq, Eq)]
        pub enum Mode {
            Honest,
            /// revealPluginToken 回 -32602：包不在册。
            NotInstalled,
            /// whoami 回一个别的主体。
            WrongPrincipal,
        }

        pub struct Server {
            pub path: std::path::PathBuf,
            /// 每条连接按顺序收到的方法名。
            pub connections: Arc<Mutex<Vec<Vec<String>>>>,
            pub mode: Arc<Mutex<Mode>>,
            /// ADR-0048：`None` = 老 daemon，`getLocalePreference` 回 `-32601`。
            pub locale: Arc<Mutex<Option<Vec<String>>>>,
            _dir: tempfile::TempDir,
        }

        pub async fn start(mode: Mode) -> Server {
            start_with_locale(mode, None).await
        }

        pub async fn start_with_locale(mode: Mode, locale: Option<Vec<String>>) -> Server {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("s.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let connections: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
            let mode = Arc::new(Mutex::new(mode));
            let locale = Arc::new(Mutex::new(locale));
            let log = connections.clone();
            let mode_ref = mode.clone();
            let locale_ref = locale.clone();
            tokio::spawn(async move {
                let issued: Arc<Mutex<Option<String>>> = Arc::default();
                let mut counter = 0_u64;
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    counter += 1;
                    let index = {
                        let mut log = log.lock().unwrap();
                        log.push(Vec::new());
                        log.len() - 1
                    };
                    let log = log.clone();
                    let issued = issued.clone();
                    let mode = mode_ref.clone();
                    let locale = locale_ref.clone();
                    tokio::spawn(async move {
                        let (read, mut write) = tokio::io::split(stream);
                        let mut read = BufReader::new(read);
                        let mut authenticated = false;
                        loop {
                            let mut line = String::new();
                            if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            let request: Value = serde_json::from_str(&line).unwrap();
                            let method = request["method"].as_str().unwrap().to_owned();
                            log.lock().unwrap()[index].push(method.clone());
                            let id = request["id"].clone();
                            let mode = *mode.lock().unwrap();
                            let current_locale = locale.lock().unwrap().clone();
                            let mut push = None;
                            let reply = match method.as_str() {
                                "getLocalePreference" if current_locale.is_some() => json!({
                                    "jsonrpc": "2.0", "id": id,
                                    "result": {
                                        "preference": {"mode": "system", "languages": []},
                                        "effective": current_locale,
                                        "persisted": true
                                    }
                                }),
                                // 真 daemon 的行为：应答之后向所有流式连接推送新的 effective。
                                "setLocalePreference" if current_locale.is_some() => {
                                    let languages = request["params"]["languages"].clone();
                                    let effective: Vec<String> =
                                        serde_json::from_value(languages).unwrap();
                                    *locale.lock().unwrap() = Some(effective.clone());
                                    push = Some(json!({
                                        "jsonrpc": "2.0",
                                        "method": "preferences/locale",
                                        "params": {"effective": effective}
                                    }));
                                    json!({"jsonrpc": "2.0", "id": id, "result": {
                                        "preference": {"mode": "explicit", "languages": effective},
                                        "effective": effective,
                                        "persisted": true
                                    }})
                                }
                                "revealPluginToken" if mode == Mode::NotInstalled => json!({
                                    "jsonrpc": "2.0", "id": id,
                                    "error": {"code": -32602, "message": "插件未安装或没有身份条目"}
                                }),
                                "revealPluginToken" => {
                                    let token = format!("tok-{counter}-{}", "x".repeat(32));
                                    *issued.lock().unwrap() = Some(token.clone());
                                    json!({"jsonrpc": "2.0", "id": id, "result": {"token": token}})
                                }
                                "authenticate" => {
                                    let ok = issued.lock().unwrap().as_deref()
                                        == request["params"]["token"].as_str();
                                    if ok {
                                        authenticated = true;
                                        json!({"jsonrpc": "2.0", "id": id, "result": {}})
                                    } else {
                                        json!({"jsonrpc": "2.0", "id": id,
                                               "error": {"code": -32014, "message": "凭证无效"}})
                                    }
                                }
                                "whoami" => {
                                    let principal = match (authenticated, mode) {
                                        (true, Mode::WrongPrincipal) => "frontend:org.other",
                                        (true, _) => "frontend:com.example.shell",
                                        (false, _) => "user",
                                    };
                                    json!({"jsonrpc": "2.0", "id": id, "result": {"principal": principal}})
                                }
                                "status" => json!({"jsonrpc": "2.0", "id": id,
                                    "result": {"version": "0.1.0", "accounts": [], "message_count": 0}}),
                                "listAccounts" => json!({"jsonrpc": "2.0", "id": id, "result": {"accounts": []}}),
                                "listMessageSummaries" => json!({"jsonrpc": "2.0", "id": id,
                                    "result": {"messages": [], "total": 0}}),
                                _ => json!({"jsonrpc": "2.0", "id": id,
                                    "error": {"code": -32601, "message": "method not found"}}),
                            };
                            for frame in std::iter::once(reply).chain(push) {
                                let mut bytes = serde_json::to_vec(&frame).unwrap();
                                bytes.push(b'\n');
                                if write.write_all(&bytes).await.is_err() {
                                    return;
                                }
                            }
                        }
                    });
                }
            });
            Server {
                path,
                connections,
                mode,
                locale,
                _dir: dir,
            }
        }
    }

    #[cfg(unix)]
    fn shell_options() -> ActorOptions {
        ActorOptions {
            identity: Some(crate::FrontendIdentity {
                frontend_id: "com.example.shell".into(),
            }),
            ..test_options()
        }
    }

    #[cfg(unix)]
    async fn wait_until(
        state: &mut watch::Receiver<ViewState>,
        mut predicate: impl FnMut(&ViewState) -> bool,
    ) -> ViewState {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if predicate(&state.borrow()) {
                    return state.borrow().clone();
                }
                state.changed().await.unwrap();
            }
        })
        .await
        .expect("状态未在 3s 内到达预期")
    }

    /// ADR-0030 决策 6 / ADR-0031 决策 3：reveal 走短命 user 连接，长连接首帧是 authenticate，
    /// 主体来自 whoami 而不是本地拼字符串。
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn frontend_identity_binds_on_a_fresh_long_connection() {
        let server = identity_server::start(identity_server::Mode::Honest).await;
        let handle = ConnectionHandle::spawn(
            Endpoint::Unix {
                path: server.path.clone(),
            },
            shell_options(),
        )
        .unwrap();
        let mut state = handle.subscribe_state();
        let online = wait_until(&mut state, |s| s.phase == ConnectionPhase::Online).await;
        assert_eq!(online.principal.as_deref(), Some("frontend:com.example.shell"));
        assert_eq!(online.identity_failure, None);

        let connections = server.connections.lock().unwrap().clone();
        assert_eq!(connections.len(), 2, "bootstrap 之后不该再有第三条连接: {connections:?}");
        // actor 先开长连接再 reveal，所以 accept 顺序是 [长, 短]；按内容而不是下标认。
        let reveal = connections
            .iter()
            .find(|c| c.first().map(String::as_str) == Some("revealPluginToken"))
            .expect("有一条只做 reveal 的短命连接");
        // 那条无法消除的 user 连接：只有 reveal，应答后即关。
        assert_eq!(reveal, &vec!["revealPluginToken".to_string()]);
        let long = connections
            .iter()
            .find(|c| c.first().map(String::as_str) == Some("authenticate"))
            .expect("长连接首帧必须是 authenticate");
        assert_eq!(long[1], "whoami");
        assert!(long[2..].iter().any(|m| m == "status"));

        handle.shutdown().await.unwrap();
    }

    /// 包不在册：失败原因单独落 identity_failure（修复动作是安装，不是等网络），并按退避重试。
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn missing_frontend_package_is_reported_and_retried_until_installed() {
        let server = identity_server::start(identity_server::Mode::NotInstalled).await;
        let handle = ConnectionHandle::spawn(
            Endpoint::Unix {
                path: server.path.clone(),
            },
            shell_options(),
        )
        .unwrap();
        let mut state = handle.subscribe_state();
        let failed = wait_until(&mut state, |s| s.identity_failure.is_some()).await;
        assert!(
            failed.identity_failure.as_deref().unwrap().contains("未安装"),
            "{failed:?}"
        );
        assert_eq!(failed.principal, None);
        assert_eq!(failed.identity_failure_code, Some(IdentityFailure::NotInstalled));
        assert_ne!(failed.phase, ConnectionPhase::Online);

        // 第二次 reveal 到达 = 在退避重试，而不是放弃。
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let reveals = server
                    .connections
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|c| c.first().map(String::as_str) == Some("revealPluginToken"))
                    .count();
                if reveals >= 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("应持续重试 reveal");

        // 用户装好包：下一次重试即绑定成功，失败原因清空。
        *server.mode.lock().unwrap() = identity_server::Mode::Honest;
        let online = wait_until(&mut state, |s| s.phase == ConnectionPhase::Online).await;
        assert_eq!(online.identity_failure, None);
        assert_eq!(online.identity_failure_code, None, "成功绑定同时清空 code");
        assert_eq!(online.principal.as_deref(), Some("frontend:com.example.shell"));
        handle.shutdown().await.unwrap();
    }

    /// whoami 回报的主体不是自己：不接受一条身份可疑的连接。
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_mismatched_principal_is_refused() {
        let server = identity_server::start(identity_server::Mode::WrongPrincipal).await;
        let handle = ConnectionHandle::spawn(
            Endpoint::Unix {
                path: server.path.clone(),
            },
            shell_options(),
        )
        .unwrap();
        let mut state = handle.subscribe_state();
        let failed = wait_until(&mut state, |s| s.identity_failure.is_some()).await;
        assert!(failed.identity_failure.as_deref().unwrap().contains("frontend:org.other"));
        assert_eq!(failed.identity_failure_code, Some(IdentityFailure::PrincipalMismatch));
        assert_eq!(failed.principal, None);
        handle.shutdown().await.unwrap();
    }

    /// 不声明身份的消费者（旧桌面壳、Menu）行为零变化：没有 reveal，没有 authenticate。
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn consumers_without_identity_still_connect_as_user() {
        let server = identity_server::start(identity_server::Mode::Honest).await;
        let handle = ConnectionHandle::spawn(
            Endpoint::Unix {
                path: server.path.clone(),
            },
            test_options(),
        )
        .unwrap();
        let mut state = handle.subscribe_state();
        let online = wait_until(&mut state, |s| s.phase == ConnectionPhase::Online).await;
        assert_eq!(online.principal, None);
        let connections = server.connections.lock().unwrap().clone();
        assert_eq!(connections.len(), 1);
        assert!(!connections[0].iter().any(|m| m == "authenticate" || m == "revealPluginToken"));
        handle.shutdown().await.unwrap();
    }

    /// ADR-0048：每条新连接的首个快照带上 `getLocalePreference`；之后 `preferences/locale` 推送
    /// 即时更新 `locale` 并广播事件。
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn locale_preference_is_fetched_on_connect_and_follows_pushes() {
        let server = identity_server::start_with_locale(
            identity_server::Mode::Honest,
            Some(vec!["zh-Hans-CN".into(), "en-US".into()]),
        )
        .await;
        let handle = ConnectionHandle::spawn(
            Endpoint::Unix {
                path: server.path.clone(),
            },
            shell_options(),
        )
        .unwrap();
        let mut events = handle.subscribe_events();
        let mut state = handle.subscribe_state();
        let online = wait_until(&mut state, |s| s.phase == ConnectionPhase::Online).await;
        assert_eq!(
            online.locale,
            Some(vec!["zh-Hans-CN".to_string(), "en-US".to_string()])
        );
        let connections = server.connections.lock().unwrap().clone();
        let long = connections
            .iter()
            .find(|c| c.first().map(String::as_str) == Some("authenticate"))
            .unwrap();
        assert!(long.iter().any(|m| m == "getLocalePreference"), "{long:?}");

        handle
            .request(
                "setLocalePreference",
                json!({ "mode": "explicit", "languages": ["en"] }),
            )
            .await
            .unwrap();
        let switched = wait_until(&mut state, |s| {
            s.locale.as_deref() == Some(&["en".to_string()])
        })
        .await;
        assert_eq!(
            switched.phase,
            ConnectionPhase::Online,
            "推送不触发重新同步"
        );
        assert_eq!(
            server.locale.lock().unwrap().clone(),
            Some(vec!["en".to_string()])
        );
        let event = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let AppEvent::Notification(ServerNotification::LocalePreference { effective }) =
                    events.recv().await.unwrap()
                {
                    return effective;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(event, vec!["en".to_string()]);
        handle.shutdown().await.unwrap();
    }

    /// 老 daemon（`-32601`）：连接照常上线，`locale` 保持 `None`，界面停在基础语言。
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn an_old_daemon_without_locale_preference_leaves_locale_unset() {
        let server = identity_server::start(identity_server::Mode::Honest).await;
        let handle = ConnectionHandle::spawn(
            Endpoint::Unix {
                path: server.path.clone(),
            },
            shell_options(),
        )
        .unwrap();
        let mut state = handle.subscribe_state();
        let online = wait_until(&mut state, |s| s.phase == ConnectionPhase::Online).await;
        assert_eq!(online.locale, None);
        assert!(!online.partial_snapshot, "缺少语言偏好不算部分快照");
        assert_eq!(online.last_error, None);
        handle.shutdown().await.unwrap();
    }

    #[test]
    fn locale_results_update_clear_or_keep_the_preference() {
        let rpc = |code| ClientError {
            kind: ErrorKind::Rpc,
            message: "x".into(),
            rpc_code: Some(code),
            rpc_data: None,
        };
        let mut state = ViewState::new(TransportKind::Unix);
        apply_locale_result(
            &mut state,
            Ok(json!({
                "preference": { "mode": "explicit", "languages": ["en"] },
                "effective": ["en"],
                "persisted": true
            })),
        );
        assert_eq!(state.locale, Some(vec!["en".to_string()]));
        // 抖动与坏应答不把界面语言打回默认。
        apply_locale_result(
            &mut state,
            Err(ClientError::connection("JSON-RPC 请求超时")),
        );
        apply_locale_result(&mut state, Err(rpc(-32603)));
        apply_locale_result(&mut state, Ok(json!({ "effective": [] })));
        apply_locale_result(&mut state, Ok(json!({ "effective": "en" })));
        apply_locale_result(
            &mut state,
            Ok(json!({ "effective": ["x".repeat(MAX_LOCALE_TAG_BYTES + 1)] })),
        );
        assert_eq!(state.locale, Some(vec!["en".to_string()]));
        // daemon 不提供该方法：老 daemon 的 -32601，或其授权表对前端主体 fail-closed 的 -32016。
        for code in [-32601, -32016] {
            state.locale = Some(vec!["en".into()]);
            apply_locale_result(&mut state, Err(rpc(code)));
            assert_eq!(state.locale, None, "{code}");
        }
    }

    #[test]
    fn locale_push_applies_immediately_and_wins_over_the_snapshot_reply() {
        let (mut actor, _commands, state, mut events, _shutdown) = test_actor();
        let push = |effective: &[&str]| {
            Incoming::Notification(maild_client::Notification {
                method: maild_client::LOCALE_PREFERENCE_NOTIFICATION.into(),
                params: json!({ "effective": effective }),
            })
        };
        let mut pending = BTreeMap::new();
        let mut snapshot = Some(page_out_snapshot());
        let mut synchronized = false;

        actor
            .handle_incoming(
                push(&["en-GB"]),
                &mut pending,
                &mut snapshot,
                &mut synchronized,
                true,
            )
            .unwrap();
        let first = state.borrow().clone();
        assert_eq!(first.locale, Some(vec!["en-GB".to_string()]));
        assert!(matches!(
            events.try_recv(),
            Ok(AppEvent::Notification(ServerNotification::LocalePreference { effective }))
                if effective == ["en-GB"]
        ));
        let tracker = snapshot.as_ref().unwrap();
        assert!(tracker.locale_pushed);
        assert_eq!(tracker.buffered.len(), 1, "推送不进快照缓冲");

        // 同一列表重复推送：不重复发布状态。
        actor
            .handle_incoming(
                push(&["en-GB"]),
                &mut pending,
                &mut snapshot,
                &mut synchronized,
                true,
            )
            .unwrap();
        assert_eq!(state.borrow().revision, first.revision);

        // 快照里的应答（zh-Hans-CN）早于推送，收尾时不覆盖。
        actor.finish_snapshot(&mut snapshot, false, true).unwrap();
        assert_eq!(actor.state.locale, Some(vec!["en-GB".to_string()]));

        // 形状不对的推送是协议错误，与其他通知一致。
        assert!(actor
            .handle_incoming(
                push(&[]),
                &mut pending,
                &mut snapshot,
                &mut synchronized,
                true
            )
            .is_err());
    }

    #[test]
    fn agent_stopping_requests_shutdown_once_without_entering_the_snapshot_buffer() {
        let (mut actor, _commands, state, mut events, _shutdown) = test_actor();
        let stopping = || {
            Incoming::Notification(maild_client::Notification {
                method: "agent/stopping".into(),
                params: json!({}),
            })
        };
        let mut pending = BTreeMap::new();
        // 同步进行中：验证停机信号即时生效且不进缓冲。
        let mut snapshot = Some(SnapshotTracker::new());
        let mut synchronized = false;
        assert!(!state.borrow().shutdown_requested);

        actor
            .handle_incoming(
                stopping(),
                &mut pending,
                &mut snapshot,
                &mut synchronized,
                true,
            )
            .unwrap();
        let first = state.borrow().clone();
        assert!(first.shutdown_requested);
        assert!(matches!(
            events.try_recv(),
            Ok(AppEvent::Notification(ServerNotification::AgentStopping))
        ));
        assert!(
            snapshot.as_ref().unwrap().buffered.is_empty(),
            "停机信号不进快照缓冲，快照收尾不会重放"
        );

        actor
            .handle_incoming(
                stopping(),
                &mut pending,
                &mut snapshot,
                &mut synchronized,
                true,
            )
            .unwrap();
        assert_eq!(state.borrow().revision, first.revision, "重复帧不再发布");
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn notification_reducer_deduplicates_and_caps_messages() {
        let mut state = ViewState::new(TransportKind::Unix);
        state.accounts.push(crate::AccountState {
            account: "personal".into(),
            state: "online".into(),
            detail: String::new(),
            ..Default::default()
        });
        state.total_messages = 4;
        for uid in ["1", "2", "3"] {
            let at = uid.parse().unwrap();
            apply_notification(
                &mut state,
                &ServerNotification::MessageReceived(Box::new(MailMessage {
                    account: "personal".into(),
                    uid: uid.into(),
                    sender: "sender@example.com".into(),
                    recipient: String::new(),
                    subject: uid.into(),
                    body: String::new(),
                    received_at: at,
                    sort_at: at,
                    ..Default::default()
                })),
                2,
                true,
            );
        }
        assert_eq!(state.messages.len(), 2);
        assert_eq!(state.messages[0].uid, "3");
        assert_eq!(state.total_messages, 7);

        let duplicate = state.messages[0].clone();
        apply_notification(
            &mut state,
            &ServerNotification::MessageReceived(Box::new(MailMessage {
                account: duplicate.account,
                uid: duplicate.uid,
                sender: duplicate.sender,
                recipient: String::new(),
                subject: duplicate.subject,
                body: String::new(),
                received_at: duplicate.received_at,
                sort_at: duplicate.sort_at,
                ..Default::default()
            })),
            2,
            true,
        );
        assert_eq!(state.total_messages, 7);

        // Historical sync mail observed later (`received_at` = 999) with an older `sort_at`
        // must NOT jump ahead of newer `sort_at` mail.
        apply_notification(
            &mut state,
            &ServerNotification::MessageReceived(Box::new(MailMessage {
                account: "personal".into(),
                uid: "99".into(),
                sender: "sender@example.com".into(),
                recipient: String::new(),
                subject: "historical".into(),
                body: String::new(),
                received_at: 999,
                sort_at: 1,
                ..Default::default()
            })),
            3,
            true,
        );
        assert_eq!(
            state
                .messages
                .iter()
                .map(|m| m.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["3", "2", "99"]
        );

        assert!(!apply_notification(
            &mut state,
            &ServerNotification::AccountState(crate::AccountState {
                account: "unknown".into(),
                state: "online".into(),
                detail: String::new(),
                ..Default::default()
            }),
            2,
            true,
        ));
        assert_eq!(state.accounts.len(), 1);

        let oversized_page = crate::MessageSummariesResult {
            messages: (0..3)
                .map(|index| MessageSummary {
                    account: "personal".into(),
                    uid: index.to_string(),
                    sender: "sender@example.com".into(),
                    recipient: String::new(),
                    subject: "subject".into(),
                    received_at: index,
                    ..Default::default()
                })
                .collect(),
            total: 3,
        };
        assert!(validate_messages(&oversized_page, 2).is_err());

        let mut oversized_subject = oversized_page;
        oversized_subject.messages.truncate(1);
        oversized_subject.messages[0].subject = "x".repeat(MAX_NOTIFICATION_TEXT_BYTES + 1);
        assert!(validate_messages(&oversized_subject, 2).is_err());

        let mut invalid_total = oversized_subject;
        invalid_total.total = 0;
        assert!(validate_messages(&invalid_total, 2).is_err());

        let invalid_status = crate::StatusResult {
            version: "0.1.0".into(),
            accounts: Vec::new(),
            message_count: -1,
            runtime: None,
        };
        assert!(validate_status(&invalid_status).is_err());
    }

    #[test]
    fn snapshot_notification_buffer_is_bounded_and_marks_refresh() {
        let mut snapshot = SnapshotTracker::new();
        for index in 0..=MAX_BUFFERED_NOTIFICATIONS {
            snapshot.buffer(ServerNotification::AccountState(crate::AccountState {
                account: format!("account-{index}"),
                state: "online".into(),
                detail: String::new(),
                ..Default::default()
            }));
        }
        assert_eq!(snapshot.buffered.len(), MAX_BUFFERED_NOTIFICATIONS);
        assert!(snapshot.buffered_bytes <= MAX_BUFFERED_NOTIFICATION_BYTES);
        assert!(snapshot.overflowed);

        let mut byte_bounded = SnapshotTracker::new();
        for index in 0..40 {
            byte_bounded.buffer(ServerNotification::MessageReceived(Box::new(MailMessage {
                account: "personal".into(),
                uid: index.to_string(),
                sender: "sender@example.com".into(),
                recipient: String::new(),
                subject: "subject".into(),
                body: "x".repeat(MAX_NOTIFICATION_BODY_BYTES),
                received_at: index,
                ..Default::default()
            })));
        }
        assert!(byte_bounded.buffered_bytes <= MAX_BUFFERED_NOTIFICATION_BYTES);
        assert!(byte_bounded.overflowed);

        let oversized = ServerNotification::MessageReceived(Box::new(MailMessage {
            account: "personal".into(),
            uid: "1".into(),
            sender: "sender@example.com".into(),
            recipient: String::new(),
            subject: "subject".into(),
            body: "x".repeat(MAX_NOTIFICATION_BODY_BYTES + 1),
            received_at: 1,
            ..Default::default()
        }));
        assert!(validate_notification(&oversized).is_err());
    }

    #[test]
    fn snapshot_page_out_notification_does_not_double_count_total() {
        let (mut actor, _commands, _state, _events, _shutdown) = test_actor();
        let mut snapshot = Some(page_out_snapshot());

        let outcome = actor.finish_snapshot(&mut snapshot, true, false).unwrap();
        assert!(outcome.follow_up);
        assert_eq!(actor.state.total_messages, 10);
        assert!(actor
            .state
            .messages
            .iter()
            .any(|message| message.uid == "page-out"));
        assert_eq!(actor.state.phase, ConnectionPhase::Synchronizing);
    }

    #[test]
    fn automatic_follow_up_is_bounded_and_keeps_refresh_waiter() {
        let (mut actor, _commands, _state, _events, _shutdown) = test_actor();
        let (reply_tx, mut reply_rx) = oneshot::channel();
        let mut first = page_out_snapshot();
        first.waiters.push(reply_tx);
        let mut snapshot = Some(first);

        let first = actor.finish_snapshot(&mut snapshot, true, false).unwrap();
        assert!(first.follow_up);
        assert!(matches!(
            reply_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        let mut second = page_out_snapshot();
        second.waiters = first.waiters;
        snapshot = Some(second);
        let second = actor.finish_snapshot(&mut snapshot, false, false).unwrap();
        assert!(!second.follow_up);
        assert!(matches!(reply_rx.try_recv(), Ok(Err(_))));
        assert_eq!(actor.state.phase, ConnectionPhase::Online);
        assert!(actor.state.partial_snapshot);
    }

    fn page_out_snapshot() -> SnapshotTracker {
        let mut tracker = SnapshotTracker::new();
        tracker.status = Some(Ok(json!({
            "version": "0.1.0",
            "accounts": [{ "account": "personal", "state": "online", "detail": "" }],
            "message_count": 10
        })));
        tracker.accounts = Some(Ok(json!({
            "accounts": [{ "account": "personal", "state": "online", "detail": "" }]
        })));
        tracker.messages = Some(Ok(json!({
            "messages": [{
                "account": "personal",
                "uid": "newest",
                "sender": "sender@example.com",
                "subject": "newest",
                "body": "preview",
                "received_at": 10,
                "sort_at": 10
            }],
            "total": 10
        })));
        tracker.buffer(ServerNotification::MessageReceived(Box::new(MailMessage {
            account: "personal".into(),
            uid: "page-out".into(),
            sender: "sender@example.com".into(),
            recipient: String::new(),
            subject: "older".into(),
            body: "preview".into(),
            received_at: 9,
            sort_at: 9,
            ..Default::default()
        })));
        tracker.locale = Some(Ok(json!({
            "preference": { "mode": "system", "languages": [] },
            "effective": ["zh-Hans-CN"],
            "persisted": true
        })));
        tracker
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ipc_refresh_waiter_completes_after_snapshot_is_applied() {
        let (client, _server) = tokio::io::duplex(16 * 1024);
        let (_reader, mut writer) = StreamConnection::from_stream(client).into_split();
        let (mut actor, _commands, _state, _events, _shutdown) = test_actor();
        let mut pending = BTreeMap::new();
        let mut snapshot = None;
        let (reply_tx, mut reply_rx) = oneshot::channel();
        actor
            .start_snapshot(
                &mut writer,
                &mut pending,
                &mut snapshot,
                Some(Instant::now() + Duration::from_secs(1)),
                vec![reply_tx],
            )
            .await
            .unwrap();
        assert!(matches!(
            reply_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        {
            let tracker = snapshot.as_mut().unwrap();
            tracker.record(
                SnapshotPart::Status,
                Ok(json!({ "version": "0.1.0", "accounts": [], "message_count": 0 })),
            );
            tracker.record(SnapshotPart::Accounts, Ok(json!({ "accounts": [] })));
            tracker.record(
                SnapshotPart::Messages,
                Ok(json!({ "messages": [], "total": 0 })),
            );
            assert!(!tracker.is_complete(), "语言偏好也是快照的一部分");
            tracker.record(
                SnapshotPart::Locale,
                Err(ClientError {
                    kind: ErrorKind::Rpc,
                    message: "method not found".into(),
                    rpc_code: Some(-32601),
                    rpc_data: None,
                }),
            );
        }
        assert!(
            !actor
                .finish_snapshot(&mut snapshot, true, false)
                .unwrap()
                .follow_up
        );
        reply_rx.await.unwrap().unwrap();
        assert_eq!(actor.state.phase, ConnectionPhase::Online);
        assert_eq!(
            actor.state.locale, None,
            "老 daemon：不提供偏好，前端回落基础语言"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ipc_session_reconciles_notifications_and_fails_pending_on_disconnect() {
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (mut actor, commands, mut state_rx, mut events, _shutdown) = test_actor();
        let session = tokio::spawn(async move {
            actor
                .run_ipc_session(StreamConnection::from_stream(client))
                .await
        });
        let fake_server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            let mut read = BufReader::new(read);
            let mut requests = BTreeMap::new();
            for _ in 0..SNAPSHOT_REQUESTS {
                let mut line = String::new();
                read.read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                requests.insert(
                    request["method"].as_str().unwrap().to_owned(),
                    request["id"].clone(),
                );
            }
            let message = json!({
                "account": "personal",
                "uid": "7",
                "sender": "sender@example.com",
                "subject": "hello",
                "body": "preview",
                "received_at": 7
            });
            write.write_all(
                format!("{{\"jsonrpc\":\"2.0\",\"method\":\"message/received\",\"params\":{message}}}\n").as_bytes()
            ).await.unwrap();
            write.write_all(
                format!("{{\"jsonrpc\":\"2.0\",\"result\":{{\"messages\":[{message}],\"total\":1}},\"id\":{}}}\n", requests["listMessageSummaries"]).as_bytes()
            ).await.unwrap();
            write.write_all(
                format!("{{\"jsonrpc\":\"2.0\",\"result\":{{\"preference\":{{\"mode\":\"explicit\",\"languages\":[\"en\"]}},\"effective\":[\"en\"],\"persisted\":true}},\"id\":{}}}\n", requests["getLocalePreference"]).as_bytes()
            ).await.unwrap();
            write.write_all(
                format!("{{\"jsonrpc\":\"2.0\",\"result\":{{\"version\":\"0.1.0\",\"accounts\":[],\"message_count\":1}},\"id\":{}}}\n", requests["status"]).as_bytes()
            ).await.unwrap();
            write
                .write_all(
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"result\":{{\"accounts\":[{{\"account\":\"personal\",\"state\":\"online\",\"detail\":\"\"}}]}},\"id\":{}}}\n",
                        requests["listAccounts"]
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let mut line = String::new();
            read.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            write
                .write_all(
                    format!(
                        "{{\"jsonrpc\":\"2.0\",\"result\":{{\"ok\":true}},\"id\":{}}}\n",
                        request["id"]
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            line.clear();
            read.read_line(&mut line).await.unwrap();
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if state_rx.borrow().phase == ConnectionPhase::Online {
                    break;
                }
                state_rx.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(state_rx.borrow().messages.len(), 1);
        assert_eq!(state_rx.borrow().total_messages, 1);
        assert_eq!(state_rx.borrow().locale, Some(vec!["en".to_string()]));
        assert!(matches!(
            events.recv().await.unwrap(),
            AppEvent::Notification(ServerNotification::MessageReceived(message)) if message.uid == "7"
        ));

        let (expired_tx, expired_rx) = oneshot::channel();
        commands
            .send(Command::Request {
                method: "status".into(),
                params: json!({}),
                deadline: Instant::now() - Duration::from_millis(1),
                reply: expired_tx,
            })
            .await
            .unwrap();
        assert!(expired_rx
            .await
            .unwrap()
            .unwrap_err()
            .message
            .contains("超时"));

        let (reply_tx, reply_rx) = oneshot::channel();
        commands
            .send(Command::Request {
                method: "send".into(),
                params: json!({}),
                deadline: Instant::now() + Duration::from_secs(1),
                reply: reply_tx,
            })
            .await
            .unwrap();
        assert_eq!(reply_rx.await.unwrap().unwrap()["ok"], true);

        let (reply_tx, reply_rx) = oneshot::channel();
        commands
            .send(Command::Request {
                method: "status".into(),
                params: json!({}),
                deadline: Instant::now() + Duration::from_secs(1),
                reply: reply_tx,
            })
            .await
            .unwrap();
        fake_server.await.unwrap();
        let error = reply_rx.await.unwrap().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Connection);
        assert!(error.message.contains("关闭"));
        assert!(matches!(
            session.await.unwrap(),
            SessionExit::Disconnected {
                synchronized: true,
                ..
            }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_actor_retries_then_polls_a_fresh_snapshot() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut successful_requests = 0_u64;
            for request_index in 0..7 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_http_json(&mut stream).await;
                if request_index == 0 {
                    stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    continue;
                }
                successful_requests += 1;
                let round = if successful_requests <= 3 { 1 } else { 2 };
                let result = match request["method"].as_str().unwrap() {
                    "status" => {
                        json!({ "version": "0.1.0", "accounts": [], "message_count": round })
                    }
                    "listAccounts" => json!({ "accounts": [] }),
                    "listMessageSummaries" => json!({ "messages": [], "total": round }),
                    method => panic!("unexpected method {method}"),
                };
                let body =
                    json!({ "jsonrpc": "2.0", "result": result, "id": request["id"] }).to_string();
                stream.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()
                ).await.unwrap();
            }
        });
        let handle = ConnectionHandle::spawn(
            Endpoint::Http {
                url: format!("http://{address}/rpc"),
                token: "t".repeat(32),
            },
            test_options(),
        )
        .unwrap();
        let mut states = handle.subscribe_state();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let state = states.borrow().clone();
                if state.phase == ConnectionPhase::Online && state.total_messages == 2 {
                    break;
                }
                states.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(states.borrow().revision >= 5);
        assert_eq!(states.borrow().connection_epoch, 1);
        handle.shutdown().await.unwrap();
        assert_eq!(handle.state().phase, ConnectionPhase::Stopped);
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_reconnect_increments_connection_epoch() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for request_index in 0..7 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_http_json(&mut stream).await;
                if request_index == 3 {
                    stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    continue;
                }
                let recovered = request_index >= 4;
                let result = match request["method"].as_str().unwrap() {
                    "status" => json!({
                        "version": "0.1.0",
                        "accounts": [],
                        "message_count": if recovered { 2 } else { 1 }
                    }),
                    "listAccounts" => json!({ "accounts": [] }),
                    "listMessageSummaries" => json!({
                        "messages": [],
                        "total": if recovered { 2 } else { 1 }
                    }),
                    method => panic!("unexpected method {method}"),
                };
                write_http_result(&mut stream, &request, result).await;
            }
        });
        let handle = ConnectionHandle::spawn(
            Endpoint::Http {
                url: format!("http://{address}/rpc"),
                token: "t".repeat(32),
            },
            test_options(),
        )
        .unwrap();
        let mut states = handle.subscribe_state();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let state = states.borrow().clone();
                if state.phase == ConnectionPhase::Online
                    && state.connection_epoch == 2
                    && state.total_messages == 2
                {
                    break;
                }
                states.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        handle.shutdown().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_snapshot_stays_online_when_optional_services_fail() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_http_json(&mut stream).await;
                let response = match request["method"].as_str().unwrap() {
                    "status" => json!({
                        "jsonrpc": "2.0",
                        "result": {
                            "version": "0.1.0",
                            "accounts": [{ "account": "personal", "state": "offline", "detail": "" }],
                            "message_count": 3
                        },
                        "id": request["id"]
                    }),
                    "listAccounts" | "listMessageSummaries" => json!({
                        "jsonrpc": "2.0",
                        "error": { "code": -32001, "message": "service unavailable" },
                        "id": request["id"]
                    }),
                    method => panic!("unexpected method {method}"),
                };
                let body = response.to_string();
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
            }
        });
        let handle = ConnectionHandle::spawn(
            Endpoint::Http {
                url: format!("http://{address}/rpc"),
                token: "t".repeat(32),
            },
            ActorOptions {
                http_poll_interval: Duration::from_secs(60),
                ..test_options()
            },
        )
        .unwrap();
        let mut states = handle.subscribe_state();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if states.borrow().phase == ConnectionPhase::Online {
                    break;
                }
                states.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let state = states.borrow().clone();
        assert!(state.partial_snapshot);
        assert_eq!(state.accounts.len(), 1);
        assert_eq!(state.total_messages, 3);
        assert!(state.last_error.unwrap().contains("service unavailable"));
        handle.shutdown().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_connection_failure_after_status_enters_retry() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_json(&mut stream).await;
            assert_eq!(request["method"], "status");
            write_http_result(
                &mut stream,
                &request,
                json!({ "version": "0.1.0", "accounts": [], "message_count": 0 }),
            )
            .await;
        });
        let handle = ConnectionHandle::spawn(
            Endpoint::Http {
                url: format!("http://{address}/rpc"),
                token: "t".repeat(32),
            },
            ActorOptions {
                http_poll_interval: Duration::from_secs(60),
                ..test_options()
            },
        )
        .unwrap();
        let mut states = handle.subscribe_state();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(states.borrow().phase, ConnectionPhase::RetryWaiting { .. }) {
                    break;
                }
                states.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_ne!(states.borrow().phase, ConnectionPhase::Online);
        handle.shutdown().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_cancels_stalled_http_snapshot() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_http_json(&mut stream).await;
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });
        let handle = ConnectionHandle::spawn(
            Endpoint::Http {
                url: format!("http://{address}/rpc"),
                token: "t".repeat(32),
            },
            ActorOptions {
                request_timeout: Duration::from_secs(2),
                http_poll_interval: Duration::from_secs(60),
                ..test_options()
            },
        )
        .unwrap();
        accepted_rx.await.unwrap();
        tokio::time::timeout(Duration::from_millis(100), handle.shutdown())
            .await
            .expect("shutdown should preempt the stalled HTTP request")
            .unwrap();
        assert_eq!(handle.state().phase, ConnectionPhase::Stopped);
        server.abort();
    }

    async fn read_http_json(stream: &mut tokio::net::TcpStream) -> Value {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
        }
        serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
    }

    async fn write_http_result(stream: &mut tokio::net::TcpStream, request: &Value, result: Value) {
        let body = json!({ "jsonrpc": "2.0", "result": result, "id": request["id"] }).to_string();
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
    }
}
