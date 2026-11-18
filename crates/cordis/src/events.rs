use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::disposer::BoxFuture;
use crate::realm::RealmId;

/// 观察型事件（emit）：同步、按注册序调用、无返回值。
pub trait EmitEvent: 'static {
    type Args: Clone + Send + 'static;
}

/// 并发事件（parallel）：异步、并发扇出、无返回值。
pub trait ParallelEvent: 'static {
    type Args: Clone + Send + 'static;
}

/// 顺序事件（serial）：异步、按注册序、首个 `Some` 短路返回。
pub trait SerialEvent: 'static {
    type Args: Clone + Send + 'static;
    type Ret: Send + 'static;
}

/// 短路决策事件（bail）：同步、按注册序、首个 `Some` 短路返回。
pub trait BailEvent: 'static {
    type Args: Clone + Send + 'static;
    type Ret: Send + 'static;
}

/// 中间件事件（waterfall）：监听器收 `(args, next)`，`next()` 无参委托下游。
pub trait WaterfallEvent: 'static {
    type Args: Clone + Send + 'static;
    type Ret: Send + 'static;
}

/// waterfall 的 `next` 句柄：调用即委托给下一个监听器（或终端 inner）。
///
/// 注：Rust `FnOnce` 线性所有权在编译期即静态禁止重复调用 `next()`（对应上游 `#44`）。
pub type WaterfallNext<R> = Box<dyn FnOnce() -> BoxFuture<'static, R> + Send>;

/// 事件监听注册选项（对齐上游 `EventOptions`：`prepend` 前插、`global` 跨作用域全局监听）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventOptions {
    /// 若为 `true`，将监听器插入队列头部（先于已有监听器执行）。
    pub prepend: bool,
    /// 若为 `true`，忽略注册处 scope 过滤，对所有 scope 的分发均可见（在 `on_update` 中亦表示全局更新钩子）。
    pub global: bool,
}

pub(crate) type EmitL<E> = Arc<dyn Fn(<E as EmitEvent>::Args) + Send + Sync>;
pub(crate) type ParallelL<E> =
    Arc<dyn Fn(<E as ParallelEvent>::Args) -> BoxFuture<'static, ()> + Send + Sync>;
pub(crate) type SerialL<E> = Arc<
    dyn Fn(<E as SerialEvent>::Args) -> BoxFuture<'static, Option<<E as SerialEvent>::Ret>>
        + Send
        + Sync,
>;
pub(crate) type BailL<E> =
    Arc<dyn Fn(<E as BailEvent>::Args) -> Option<<E as BailEvent>::Ret> + Send + Sync>;
pub(crate) type WaterfallL<E> = Arc<
    dyn Fn(
            <E as WaterfallEvent>::Args,
            WaterfallNext<<E as WaterfallEvent>::Ret>,
        ) -> BoxFuture<'static, <E as WaterfallEvent>::Ret>
        + Send
        + Sync,
>;

type AnyListener = Box<dyn Any + Send + Sync>;

struct HookEntry {
    id: u64,
    realm: RealmId,
    global: bool,
    removed: Arc<AtomicBool>,
    listener: AnyListener,
}

pub(crate) type SnapshotItem<L> = (Arc<AtomicBool>, Arc<L>);

/// 事件总线。每种模式一个 `TypeId -> 监听器表` 映射。
///
/// 关键不变量（对齐上游 `v4.0.0-rc.13`）：
/// 1. 锁外调用用户代码：所有分发只快照 `(removed, Arc<L>)`，绝不持锁运行回调或 `.await`；
/// 2. 分发期动态移除即时生效（`#242`）：移除监听器时原子置位 `removed = true`，活跃分发在调用前跳过已移除项，
///    而分发期新增的监听器推迟到下一次分发；
/// 3. 空桶自动清理（`#51`）：最后一个监听器注销后立即删除空 `TypeId` 桶，避免长生命周期进程内存膨胀。
#[derive(Default)]
pub(crate) struct EventBus {
    emit: HashMap<TypeId, Vec<HookEntry>>,
    parallel: HashMap<TypeId, Vec<HookEntry>>,
    serial: HashMap<TypeId, Vec<HookEntry>>,
    bail: HashMap<TypeId, Vec<HookEntry>>,
    waterfall: HashMap<TypeId, Vec<HookEntry>>,
    next_id: u64,
}

fn push_with_flag<E: 'static>(
    map: &mut HashMap<TypeId, Vec<HookEntry>>,
    next_id: &mut u64,
    realm: RealmId,
    options: EventOptions,
    removed: Arc<AtomicBool>,
    listener: AnyListener,
) -> u64 {
    let id = *next_id;
    *next_id += 1;
    let entry = HookEntry {
        id,
        realm,
        global: options.global,
        removed,
        listener,
    };
    let list = map.entry(TypeId::of::<E>()).or_default();
    if options.prepend {
        list.insert(0, entry);
    } else {
        list.push(entry);
    }
    id
}

fn remove(map: &mut HashMap<TypeId, Vec<HookEntry>>, type_id: TypeId, id: u64) -> bool {
    let Some(list) = map.get_mut(&type_id) else {
        return false;
    };
    let mut found = false;
    list.retain(|entry| {
        if entry.id == id {
            entry.removed.store(true, Ordering::Release);
            found = true;
            false
        } else {
            true
        }
    });
    if list.is_empty() {
        map.remove(&type_id);
    }
    found
}

fn take_listeners<E: 'static, L: Any + Send + Sync + ?Sized>(
    map: &HashMap<TypeId, Vec<HookEntry>>,
    realm: RealmId,
) -> Vec<SnapshotItem<L>> {
    map.get(&TypeId::of::<E>())
        .map(|list| {
            list.iter()
                .filter(|entry| {
                    !entry.removed.load(Ordering::Acquire)
                        && (entry.global || entry.realm == realm || entry.realm == RealmId::ROOT)
                })
                .map(|entry| {
                    let cb = entry
                        .listener
                        .downcast_ref::<Arc<L>>()
                        .expect("listener type mismatch")
                        .clone();
                    (Arc::clone(&entry.removed), cb)
                })
                .collect()
        })
        .unwrap_or_default()
}

impl EventBus {
    pub(crate) fn on_emit_with<E: EmitEvent>(
        &mut self,
        realm: RealmId,
        options: EventOptions,
        removed: Arc<AtomicBool>,
        f: impl Fn(E::Args) + Send + Sync + 'static,
    ) -> u64 {
        let listener: EmitL<E> = Arc::new(f);
        push_with_flag::<E>(
            &mut self.emit,
            &mut self.next_id,
            realm,
            options,
            removed,
            Box::new(listener),
        )
    }

    pub(crate) fn remove_emit<E: EmitEvent>(&mut self, id: u64) -> bool {
        remove(&mut self.emit, TypeId::of::<E>(), id)
    }

    pub(crate) fn on_parallel_with<E: ParallelEvent>(
        &mut self,
        realm: RealmId,
        options: EventOptions,
        removed: Arc<AtomicBool>,
        f: impl Fn(E::Args) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> u64 {
        let listener: ParallelL<E> = Arc::new(f);
        push_with_flag::<E>(
            &mut self.parallel,
            &mut self.next_id,
            realm,
            options,
            removed,
            Box::new(listener),
        )
    }

    pub(crate) fn remove_parallel<E: ParallelEvent>(&mut self, id: u64) -> bool {
        remove(&mut self.parallel, TypeId::of::<E>(), id)
    }

    pub(crate) fn on_serial_with<E: SerialEvent>(
        &mut self,
        realm: RealmId,
        options: EventOptions,
        removed: Arc<AtomicBool>,
        f: impl Fn(E::Args) -> BoxFuture<'static, Option<E::Ret>> + Send + Sync + 'static,
    ) -> u64 {
        let listener: SerialL<E> = Arc::new(f);
        push_with_flag::<E>(
            &mut self.serial,
            &mut self.next_id,
            realm,
            options,
            removed,
            Box::new(listener),
        )
    }

    pub(crate) fn remove_serial<E: SerialEvent>(&mut self, id: u64) -> bool {
        remove(&mut self.serial, TypeId::of::<E>(), id)
    }

    pub(crate) fn on_bail_with<E: BailEvent>(
        &mut self,
        realm: RealmId,
        options: EventOptions,
        removed: Arc<AtomicBool>,
        f: impl Fn(E::Args) -> Option<E::Ret> + Send + Sync + 'static,
    ) -> u64 {
        let listener: BailL<E> = Arc::new(f);
        push_with_flag::<E>(
            &mut self.bail,
            &mut self.next_id,
            realm,
            options,
            removed,
            Box::new(listener),
        )
    }

    pub(crate) fn remove_bail<E: BailEvent>(&mut self, id: u64) -> bool {
        remove(&mut self.bail, TypeId::of::<E>(), id)
    }

    pub(crate) fn on_waterfall_with<E: WaterfallEvent>(
        &mut self,
        realm: RealmId,
        options: EventOptions,
        removed: Arc<AtomicBool>,
        f: impl Fn(E::Args, WaterfallNext<E::Ret>) -> BoxFuture<'static, E::Ret>
            + Send
            + Sync
            + 'static,
    ) -> u64 {
        let listener: WaterfallL<E> = Arc::new(f);
        push_with_flag::<E>(
            &mut self.waterfall,
            &mut self.next_id,
            realm,
            options,
            removed,
            Box::new(listener),
        )
    }

    pub(crate) fn remove_waterfall<E: WaterfallEvent>(&mut self, id: u64) -> bool {
        remove(&mut self.waterfall, TypeId::of::<E>(), id)
    }

    /// 检查指定事件类型在总线中是否仍有活跃桶（用于验证空桶回收 `#51`）。
    pub(crate) fn has_bucket<E: 'static>(&self) -> bool {
        let tid = TypeId::of::<E>();
        self.emit.contains_key(&tid)
            || self.parallel.contains_key(&tid)
            || self.serial.contains_key(&tid)
            || self.bail.contains_key(&tid)
            || self.waterfall.contains_key(&tid)
    }

    // —— 监听器快照取出（按 realm / global 过滤）：用户代码与 await 一律在锁外 ——

    pub(crate) fn emit_listeners<E: EmitEvent>(
        &self,
        realm: RealmId,
    ) -> Vec<SnapshotItem<EmitFn<E>>> {
        take_listeners::<E, EmitFn<E>>(&self.emit, realm)
    }

    pub(crate) fn bail_listeners<E: BailEvent>(
        &self,
        realm: RealmId,
    ) -> Vec<SnapshotItem<BailFn<E>>> {
        take_listeners::<E, BailFn<E>>(&self.bail, realm)
    }

    pub(crate) fn parallel_listeners<E: ParallelEvent>(
        &self,
        realm: RealmId,
    ) -> Vec<SnapshotItem<ParallelFn<E>>> {
        take_listeners::<E, ParallelFn<E>>(&self.parallel, realm)
    }

    pub(crate) fn serial_listeners<E: SerialEvent>(
        &self,
        realm: RealmId,
    ) -> Vec<SnapshotItem<SerialFn<E>>> {
        take_listeners::<E, SerialFn<E>>(&self.serial, realm)
    }

    pub(crate) fn waterfall_listeners<E: WaterfallEvent>(
        &self,
        realm: RealmId,
    ) -> Vec<SnapshotItem<WaterfallFn<E>>> {
        take_listeners::<E, WaterfallFn<E>>(&self.waterfall, realm)
    }
}

type EmitFn<E> = dyn Fn(<E as EmitEvent>::Args) + Send + Sync;
type BailFn<E> = dyn Fn(<E as BailEvent>::Args) -> Option<<E as BailEvent>::Ret> + Send + Sync;
type ParallelFn<E> =
    dyn Fn(<E as ParallelEvent>::Args) -> BoxFuture<'static, ()> + Send + Sync;
type SerialFn<E> = dyn Fn(<E as SerialEvent>::Args) -> BoxFuture<'static, Option<<E as SerialEvent>::Ret>>
    + Send
    + Sync;
type WaterfallFn<E> = dyn Fn(
        <E as WaterfallEvent>::Args,
        WaterfallNext<<E as WaterfallEvent>::Ret>,
    ) -> BoxFuture<'static, <E as WaterfallEvent>::Ret>
    + Send
    + Sync;
