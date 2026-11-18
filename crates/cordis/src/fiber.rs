use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use futures::channel::oneshot;

use crate::disposer::{BoxFuture, Disposer};
use crate::events::WaterfallNext;
use crate::plugin::{ComponentError, CordisError, ErasedComponent, InterceptFn};
use crate::realm::RealmId;
use crate::registry::ImplRecord;

/// 组件生命周期状态（对应论文 §4.3.3 的惯性状态机及上游 `v4.0.0-rc.13` `FiberState`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiberState {
    /// 等待依赖（`inject` 未全部就绪；对应上游 `FiberState.PENDING`）
    Idle,
    /// 依赖就绪，`apply` 已成功运行（对应上游 `FiberState.ACTIVE`）
    Active,
    /// `apply` 或配置校验失败（对应上游 `FiberState.FAILED`；不因依赖刷新自动重入，需 `update`/`restart`/`replace` 恢复）
    Failed,
    /// 卸载中（正在等待下游依赖排空并执行逆；对应上游 `FiberState.UNLOADING`）
    Unloading,
    /// 已彻底卸载（对应上游 `FiberState.DISPOSED`）
    Disposed,
}

impl FiberState {
    /// 上游 `FiberState.PENDING` 的同义常量别名。
    pub const PENDING: Self = Self::Idle;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Epoch {
    pub(crate) revision: u64,
    pub(crate) impl_uids: Vec<(TypeId, u64)>,
}

pub(crate) type ErasedUpdateFn = Arc<
    dyn Fn(Arc<dyn Any + Send + Sync>, WaterfallNext<()>) -> BoxFuture<'static, ()> + Send + Sync,
>;

#[derive(Clone)]
pub(crate) struct UpdateHookEntry {
    pub(crate) id: u64,
    pub(crate) removed: Arc<AtomicBool>,
    pub(crate) callback: ErasedUpdateFn,
}

/// Fiber 内部可变元数据（单叶锁保护，绝不嵌套获取其他锁或跨回调/await 持有）。
pub(crate) struct FiberMeta {
    pub(crate) state: FiberState,
    pub(crate) revision: u64,
    pub(crate) epoch: Option<Epoch>,
    pub(crate) error: Option<CordisError>,
    pub(crate) component: Option<Arc<dyn ErasedComponent>>,
    pub(crate) runtime_key: Option<usize>,
    pub(crate) inject: Vec<TypeId>,
    pub(crate) inject_intercepts: HashMap<TypeId, InterceptFn>,
    pub(crate) config: Option<Arc<dyn Any + Send + Sync>>,
    /// 当前加载周期已提交的依赖绑定（贯穿 `Active` 与 `Unloading` 阶段，直到本 fiber 自身逆全部执行完毕才清空）。
    pub(crate) committed: HashMap<TypeId, Arc<ImplRecord>>,
    /// 是否正在运行同步 `apply`（用于延迟向外部依赖者广播 `provide`，防止失败 `apply` 污染下游）。
    pub(crate) applying: bool,
    /// 是否已被显式请求永久 `dispose()`。
    pub(crate) disposing_permanently: bool,
    /// 是否有异步转换（卸载/重载）正在飞行中。
    pub(crate) in_flight: bool,
    /// 等待飞行中异步转换结束的唤醒队列。
    pub(crate) idle_waiters: Vec<oneshot::Sender<()>>,
    /// 同步路径（如 `replace_component` 遇到含异步逆的活跃组件）挂起的待驱动转换任务。
    pub(crate) deferred_task: Option<BoxFuture<'static, ()>>,
}

/// 组件运行时句柄（内部）。
pub(crate) struct Fiber {
    /// `0` 为根上下文 fiber，`>= 1` 为挂载的组件 fiber。
    pub(crate) id: u64,
    pub(crate) parent: Option<Arc<Fiber>>,
    /// 挂载时的 per-key realm 表（论文 `@@isolate` 的 `𝜌`）。
    pub(crate) realms: HashMap<TypeId, RealmId>,
    /// 挂载时的 per-key 隔离代数（用于检测哪怕绑定相同 realm 的显式 `isolate` 边界，`#209`）。
    pub(crate) isolation_epochs: HashMap<TypeId, u64>,
    pub(crate) meta: Mutex<FiberMeta>,
    pub(crate) disposers: Mutex<Vec<Disposer>>,
    /// 本 fiber 提供的服务记录（包含尚未排空完毕的已撤回绑定，直到 `dependents` 归零且本 fiber 清理完成才移除）。
    pub(crate) provisions: Mutex<Vec<Arc<ImplRecord>>>,
    /// fiber 作用域的 `on_update` 钩子表（随重载自动清理，不跨轮次累积，`#56`）。
    pub(crate) update_hooks: Mutex<Vec<UpdateHookEntry>>,
}

impl Fiber {
    pub(crate) fn new_root() -> Arc<Self> {
        Arc::new(Self {
            id: 0,
            parent: None,
            realms: HashMap::new(),
            isolation_epochs: HashMap::new(),
            meta: Mutex::new(FiberMeta {
                state: FiberState::Active,
                revision: 0,
                epoch: Some(Epoch {
                    revision: 0,
                    impl_uids: Vec::new(),
                }),
                error: None,
                component: None,
                runtime_key: None,
                inject: Vec::new(),
                inject_intercepts: HashMap::new(),
                config: None,
                committed: HashMap::new(),
                applying: false,
                disposing_permanently: false,
                in_flight: false,
                idle_waiters: Vec::new(),
                deferred_task: None,
            }),
            disposers: Mutex::new(Vec::new()),
            provisions: Mutex::new(Vec::new()),
            update_hooks: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn new_component(
        id: u64,
        parent: Arc<Fiber>,
        realms: HashMap<TypeId, RealmId>,
        isolation_epochs: HashMap<TypeId, u64>,
        component: Arc<dyn ErasedComponent>,
        runtime_key: Option<usize>,
        config: Option<Arc<dyn Any + Send + Sync>>,
    ) -> Arc<Self> {
        let inject = component.inject();
        let inject_intercepts = component.inject_intercepts();
        Arc::new(Self {
            id,
            parent: Some(parent),
            realms,
            isolation_epochs,
            meta: Mutex::new(FiberMeta {
                state: FiberState::Idle,
                revision: 0,
                epoch: None,
                error: None,
                component: Some(component),
                runtime_key,
                inject,
                inject_intercepts,
                config,
                committed: HashMap::new(),
                applying: false,
                disposing_permanently: false,
                in_flight: false,
                idle_waiters: Vec::new(),
                deferred_task: None,
            }),
            disposers: Mutex::new(Vec::new()),
            provisions: Mutex::new(Vec::new()),
            update_hooks: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn is_root(&self) -> bool {
        self.id == 0
    }

    /// 该 fiber 眼中 key 的 realm（未隔离的 key 归 ROOT，即共享全局绑定）。
    pub(crate) fn realm_of(&self, type_id: TypeId) -> RealmId {
        self.realms.get(&type_id).copied().unwrap_or(RealmId::ROOT)
    }

    pub(crate) fn isolation_epoch_of(&self, type_id: TypeId) -> Option<u64> {
        self.isolation_epochs.get(&type_id).copied()
    }

    /// 上下文是否仍处于活跃可用态（根 fiber 恒活跃；子组件一旦进入永久 `dispose()` 即失活）。
    pub(crate) fn is_active_context(&self) -> bool {
        if self.is_root() {
            return true;
        }
        let meta = self.meta.lock().unwrap();
        !meta.disposing_permanently && meta.state != FiberState::Disposed
    }

    pub(crate) fn check_active(&self) -> Result<(), CordisError> {
        if self.is_active_context() {
            Ok(())
        } else {
            Err(CordisError::inactive_effect())
        }
    }

    pub(crate) fn state(&self) -> FiberState {
        self.meta.lock().unwrap().state
    }

    pub(crate) fn error(&self) -> Option<CordisError> {
        self.meta.lock().unwrap().error.clone()
    }

    pub(crate) fn name(&self) -> &'static str {
        let mut cur: Option<&Fiber> = Some(self);
        while let Some(f) = cur {
            if let Some(comp) = f.meta.lock().unwrap().component.clone() {
                if let Some(name) = comp.name() {
                    return name;
                }
            }
            cur = f.parent.as_deref();
        }
        "root"
    }

    /// 等待当前飞行中的异步转换完成（若有挂起的惰性 `deferred_task` 则在此直接驱动完成）。
    pub(crate) async fn wait_idle(&self) {
        loop {
            let (deferred, waiter) = {
                let mut meta = self.meta.lock().unwrap();
                if let Some(task) = meta.deferred_task.take() {
                    (Some(task), None)
                } else if meta.in_flight {
                    let (tx, rx) = oneshot::channel();
                    meta.idle_waiters.push(tx);
                    (None, Some(rx))
                } else {
                    return;
                }
            };
            if let Some(task) = deferred {
                task.await;
            } else if let Some(rx) = waiter {
                let _ = rx.await;
            }
        }
    }

    pub(crate) fn finish_in_flight(&self) {
        let waiters = {
            let mut meta = self.meta.lock().unwrap();
            meta.in_flight = false;
            std::mem::take(&mut meta.idle_waiters)
        };
        for tx in waiters {
            let _ = tx.send(());
        }
    }

    pub(crate) fn check_result(&self) -> Result<(), ComponentError> {
        match self.error() {
            Some(err) => Err(Box::new(err)),
            None => Ok(()),
        }
    }
}
