//! 外部 transport provider 的反向调用缝（M4-05，ADR-0021）。
//!
//! 守护进程在**已认证且已注册**的插件长连接上发起 JSON-RPC 请求并等待应答：
//! 每条连接一张出站 pending 表（serve 读循环按 id 路由应答），注册表按插件 id
//! 保存活跃端点（latest-wins，连接关闭按属主注销）。未注册/掉线一律快速失败，
//! 绝不静默回退内置 transport。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cordis::Service;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

/// 错误码（接续 ADR-0019 的 -32017）。
pub const PROVIDER_UNAVAILABLE: i64 = -32018;
pub const PROVIDER_TIMEOUT: i64 = -32019;
pub const PROVIDER_ERROR: i64 = -32020;

/// 反向调用超时预算（ADR-0021 §4）。receive 分批拉取由调用方在总预算内循环。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
pub const SEND_TIMEOUT: Duration = Duration::from_secs(30);
pub const RECEIVE_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// hermetic 测试缝：设置后覆盖全部反向调用预算（毫秒）。每次调用现读——
/// 反向调用频率极低，且免去初始化顺序约束。
/// 仅测试进程使用；生产配置绝不设置，文档与 CI 均不注入。
fn budget(default: Duration) -> Duration {
    match std::env::var("MAILD_TEST_PROVIDER_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => default,
    }
}

pub(crate) fn probe_budget() -> Duration {
    budget(PROBE_TIMEOUT)
}

pub(crate) fn send_budget() -> Duration {
    budget(SEND_TIMEOUT)
}

pub(crate) fn receive_call_budget() -> Duration {
    budget(RECEIVE_CALL_TIMEOUT)
}

/// 反向调用的单条应答。
#[derive(Clone, Debug)]
pub(crate) enum ProviderReply {
    Ok(Value),
    Err { code: i64, message: String },
}

impl ProviderReply {
    pub(crate) fn unavailable(message: &str) -> Self {
        Self::Err {
            code: PROVIDER_UNAVAILABLE,
            message: message.to_owned(),
        }
    }
}

/// 入站帧按反向调用应答解析的结果（ADR-0021 / ADR-0050）。
///
/// 必须显式区分「非应答帧」与「已丢弃的迟到应答」：后者绝不能落回请求解析并回发错误帧。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResponseRouteOutcome {
    /// 不是 JSON-RPC 应答帧（例如请求、通知或非应答格式），交由后续请求解析处理。
    NotResponse,
    /// 命中本会话的未决反向调用并已投递给等待方。
    Routed,
    /// 合法的 JSON-RPC 应答帧（无 `method`、有非空 `id` 与 `result`/`error`），但对应 pending
    /// 已因超时/取消/撤销而移除；已静默丢弃并计数，绝不落入请求解析。
    DroppedLateResponse,
}

/// 同一条插件长连接上不同角色的属主标识。
///
/// 双角色连接（同时注册 `transport_provider` 与 `translation_provider`）共享同一份数字
/// `next_id` 分配器与 pending 表，但替换/撤销某一角色时只让该角色对应属主的未决调用失败，
/// 绝不误伤同一连接上的另一角色。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum CallOwner {
    Transport(u64),
    Translation(u64),
}

static NEXT_TRANSPORT_OWNER_ID: AtomicU64 = AtomicU64::new(1);

/// 每条连接的出站请求状态：id 分配 + pending 表。
///
/// 属于会话（[`crate::api::SessionAuth`]）而非端点：各角色端点借用同一张表与同一个
/// 递增数字 id 分配器，按 `CallOwner` 隔离失效范围。普通客户端请求从不进入这张表——
/// 只有守护进程主动外呼才登记。
pub(crate) struct SessionCallState {
    pending: Mutex<HashMap<String, (CallOwner, oneshot::Sender<ProviderReply>)>>,
    next_id: AtomicU64,
}

impl SessionCallState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
    }

    /// 在本连接上分配下一个单调递增的数字 JSON-RPC id（跨角色共享，避免碰撞）。
    pub(crate) fn allocate_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// serve 读循环：把带 result/error 的入站帧路由到未决调用。
    /// 返回 false 表示无此 pending（迟到应答）。
    pub(crate) fn route(&self, id: &str, reply: ProviderReply) -> bool {
        if let Some((_, sender)) = self.pending.lock().unwrap().remove(id) {
            let _ = sender.send(reply);
            true
        } else {
            false
        }
    }

    /// 仅让属于 `owner` 的未决调用立即失败（-32018），不影响同连接上的其他角色。
    pub(crate) fn fail_owner(&self, owner: CallOwner, message: &str) {
        let mut pending = self.pending.lock().unwrap();
        let keys: Vec<String> = pending
            .iter()
            .filter(|(_, (entry_owner, _))| *entry_owner == owner)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            if let Some((_, sender)) = pending.remove(&key) {
                let _ = sender.send(ProviderReply::unavailable(message));
            }
        }
    }

    /// 整条连接失效（EOF/强制断开）时让全部未决调用立即失败（-32018）。
    /// 迟到的真实应答此后落入「未知 id」丢弃。
    pub(crate) fn fail_all(&self, message: &str) {
        let mut pending = self.pending.lock().unwrap();
        for (_, (_, sender)) in pending.drain() {
            let _ = sender.send(ProviderReply::unavailable(message));
        }
    }

    pub(crate) fn insert_owned(
        &self,
        id: &str,
        owner: CallOwner,
        sender: oneshot::Sender<ProviderReply>,
    ) {
        self.pending
            .lock()
            .unwrap()
            .insert(id.to_owned(), (owner, sender));
    }

    pub(crate) fn remove(&self, id: &str) {
        self.pending.lock().unwrap().remove(id);
    }
}

/// 一个已注册 provider 端点：输出句柄指向所属会话的写任务。
pub(crate) struct ProviderEndpoint {
    plugin_id: String,
    owner_id: u64,
    output: mpsc::Sender<String>,
    calls: Arc<SessionCallState>,
    /// 单飞：同一连接同时至多一条在途 transport 反向调用（与客户端暂存上限对齐）。
    call_lock: tokio::sync::Mutex<()>,
}

/// 反向调用的失败分类（映射见 api 侧错误码）。
#[derive(Debug)]
pub(crate) enum ProviderCallError {
    /// 无注册、连接已断、被顶替。
    Unavailable(String),
    /// 超过调用预算。
    Timeout,
    /// 插件应答的业务失败（code/message 来自插件，调用方负责脱敏与有界化）。
    Upstream { code: i64, message: String },
}

impl ProviderEndpoint {
    pub(crate) fn new(
        plugin_id: String,
        output: mpsc::Sender<String>,
        calls: Arc<SessionCallState>,
    ) -> Self {
        Self {
            plugin_id,
            owner_id: NEXT_TRANSPORT_OWNER_ID.fetch_add(1, Ordering::Relaxed),
            output,
            calls,
            call_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub(crate) fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub(crate) fn owner(&self) -> CallOwner {
        CallOwner::Transport(self.owner_id)
    }

    pub(crate) fn fail_pending(&self, message: &str) {
        self.calls.fail_owner(self.owner(), message);
    }

    #[cfg(test)]
    pub(crate) fn calls(&self) -> &Arc<SessionCallState> {
        &self.calls
    }

    /// 发起一次反向调用。串行（call_lock）；超时移除 pending，迟到应答被丢弃。
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ProviderCallError> {
        let _guard = self.call_lock.lock().await;
        let id = self.calls.allocate_id();
        let key = id.to_string();
        let (tx, rx) = oneshot::channel();
        self.calls.insert_owned(&key, self.owner(), tx);
        struct PendingGuard<'a> {
            calls: &'a SessionCallState,
            key: String,
        }
        impl Drop for PendingGuard<'_> {
            fn drop(&mut self) {
                self.calls.remove(&self.key);
            }
        }
        let _pending = PendingGuard {
            calls: &self.calls,
            key: key.clone(),
        };

        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if self.output.send(frame.to_string()).await.is_err() {
            self.calls.remove(&key);
            return Err(ProviderCallError::Unavailable("provider 连接已关闭".into()));
        }
        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.calls.remove(&key);
                Err(ProviderCallError::Timeout)
            }
            Ok(Err(_dropped)) => Err(ProviderCallError::Unavailable(
                "provider 端点已失效".into(),
            )),
            Ok(Ok(ProviderReply::Ok(value))) => Ok(value),
            Ok(Ok(ProviderReply::Err { code, message })) => {
                Err(ProviderCallError::Upstream { code, message })
            }
        }
    }
}

/// provider 注册表：plugin_id → 活跃端点。Context 服务。
pub struct ProviderRegistry {
    endpoints: Mutex<BTreeMap<String, Arc<ProviderEndpoint>>>,
    late_responses: AtomicU64,
}

impl Service for ProviderRegistry {
    fn name(&self) -> &'static str {
        "provider.registry"
    }
}

impl ProviderRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            endpoints: Mutex::new(BTreeMap::new()),
            late_responses: AtomicU64::new(0),
        })
    }

    /// 登记端点；返回被顶替的旧端点（调用方负责让其未决调用失败并审计）。
    pub(crate) fn register(
        &self,
        endpoint: Arc<ProviderEndpoint>,
    ) -> Option<Arc<ProviderEndpoint>> {
        self.endpoints
            .lock()
            .unwrap()
            .insert(endpoint.plugin_id().to_owned(), endpoint)
    }

    pub(crate) fn endpoint_of(&self, plugin_id: &str) -> Option<Arc<ProviderEndpoint>> {
        self.endpoints.lock().unwrap().get(plugin_id).cloned()
    }

    /// 连接关闭时按属主注销：只有当前端点仍是本连接的端点才移除，
    /// 避免顶替后旧连接的清理误删新端点。返回是否确实移除。
    pub(crate) fn remove_if_owner(
        &self,
        plugin_id: &str,
        endpoint: &Arc<ProviderEndpoint>,
    ) -> bool {
        let mut endpoints = self.endpoints.lock().unwrap();
        match endpoints.get(plugin_id) {
            Some(current) if Arc::ptr_eq(current, endpoint) => {
                endpoints.remove(plugin_id);
                true
            }
            _ => false,
        }
    }

    /// 迟到应答计数（观测用；不进协议响应）。
    pub(crate) fn note_late_response(&self) {
        self.late_responses.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn late_responses(&self) -> u64 {
        self.late_responses.load(Ordering::Relaxed)
    }

    /// 插件 id 当前是否持有活跃注册（listExternalPlugins 观测位）。
    pub fn is_registered(&self, plugin_id: &str) -> bool {
        self.endpoints.lock().unwrap().contains_key(plugin_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_pair() -> (mpsc::Sender<String>, mpsc::Receiver<String>) {
        mpsc::channel(8)
    }

    fn endpoint(output: mpsc::Sender<String>) -> Arc<ProviderEndpoint> {
        Arc::new(ProviderEndpoint::new(
            "org.example.provider".into(),
            output,
            SessionCallState::new(),
        ))
    }

    #[test]
    fn register_supersedes_and_remove_respects_ownership() {
        let registry = ProviderRegistry::new();
        let (tx_a, _rx_a) = channel_pair();
        let (tx_b, _rx_b) = channel_pair();
        let first = endpoint(tx_a);
        let second = endpoint(tx_b);

        assert!(registry.register(first.clone()).is_none());
        let displaced = registry.register(second.clone()).expect("顶替返回旧端点");
        assert!(Arc::ptr_eq(&displaced, &first));
        assert!(registry.is_registered("org.example.provider"));

        // 旧连接的清理不得误删新端点。
        assert!(!registry.remove_if_owner("org.example.provider", &first));
        assert!(registry.remove_if_owner("org.example.provider", &second));
        assert!(!registry.is_registered("org.example.provider"));
    }

    #[tokio::test]
    async fn aborting_an_outer_send_drops_the_pending_entry() {
        let calls = SessionCallState::new();
        let (output, mut frames) = mpsc::channel(2);
        let endpoint = Arc::new(ProviderEndpoint::new(
            "fixture".into(),
            output,
            calls.clone(),
        ));
        let task = tokio::spawn(async move {
            endpoint
                .call("transport/send", json!({}), Duration::from_secs(60))
                .await
        });
        frames
            .recv()
            .await
            .expect("request must actually be in flight");
        assert_eq!(calls.pending.lock().unwrap().len(), 1);
        task.abort();
        let _ = task.await;
        assert!(calls.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn fail_all_resolves_pending_calls_as_unavailable() {
        let (tx, _rx) = channel_pair();
        let ep = endpoint(tx);
        let calls = ep.calls().clone();

        // 直接往 pending 表登记一个等待者来模拟在途调用。
        let (reply_tx, reply_rx) = oneshot::channel();
        calls.insert_owned("42", ep.owner(), reply_tx);

        calls.fail_all("superseded");
        match reply_rx.await.expect("等待者收到应答") {
            ProviderReply::Err { code, message } => {
                assert_eq!(code, PROVIDER_UNAVAILABLE);
                assert_eq!(message, "superseded");
            }
            other => panic!("期望不可用失败，实际 {other:?}"),
        }
        // 之后迟到的应答找不到 pending。
        assert!(!calls.route("42", ProviderReply::Ok(json!({}))));
    }

    #[tokio::test]
    async fn fail_owner_isolates_transport_and_translation_roles_on_shared_connection() {
        let calls = SessionCallState::new();
        let (tx_transport, mut rx_transport) = oneshot::channel();
        let (tx_translation, mut rx_translation) = oneshot::channel();
        calls.insert_owned("1", CallOwner::Transport(10), tx_transport);
        calls.insert_owned("2", CallOwner::Translation(20), tx_translation);

        calls.fail_owner(CallOwner::Transport(10), "transport replaced");
        match rx_transport.try_recv().unwrap() {
            ProviderReply::Err { code, message } => {
                assert_eq!(code, PROVIDER_UNAVAILABLE);
                assert_eq!(message, "transport replaced");
            }
            other => panic!("expected transport failure, got {other:?}"),
        }
        assert!(
            rx_translation.try_recv().is_err(),
            "translation role must remain intact when transport role is replaced"
        );
        assert!(calls.route("2", ProviderReply::Ok(json!({ "ok": true }))));
        assert!(matches!(
            rx_translation.try_recv().unwrap(),
            ProviderReply::Ok(_)
        ));
    }

    #[tokio::test]
    async fn call_times_out_and_drops_late_reply() {
        let (_tx, mut rx) = channel_pair();
        let ep = endpoint(_tx.clone());
        let registry = ProviderRegistry::new();
        registry.register(ep.clone());

        let waiter = tokio::spawn(async move {
            ep.call("transport/probe", json!({}), Duration::from_millis(30))
                .await
        });
        let frame = rx.recv().await.expect("请求帧");
        assert!(frame.contains("\"method\":\"transport/probe\""));

        // 超时后才回复：调用方拿到 Timeout，迟到应答计入丢弃。
        tokio::time::sleep(Duration::from_millis(60)).await;
        let outcome = waiter.await.unwrap();
        assert!(matches!(outcome, Err(ProviderCallError::Timeout)));
        let calls = registry.endpoint_of("org.example.provider").unwrap();
        assert!(!calls.calls().route("1", ProviderReply::Ok(json!({}))));
        registry.note_late_response();
        assert_eq!(registry.late_responses(), 1);
    }

    #[tokio::test]
    async fn closed_output_reports_unavailable() {
        let (tx, rx) = channel_pair();
        drop(rx);
        let ep = endpoint(tx);
        let outcome = ep
            .call("transport/send", json!({}), Duration::from_secs(1))
            .await;
        assert!(matches!(outcome, Err(ProviderCallError::Unavailable(_))));
    }
}
