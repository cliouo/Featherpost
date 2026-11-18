use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::disposer::{BoxFuture, Disposer};
use crate::events::{
    BailEvent, EmitEvent, EventBus, EventOptions, ParallelEvent, SerialEvent, WaterfallEvent,
    WaterfallNext,
};
use crate::fiber::{Epoch, ErasedUpdateFn, Fiber, FiberState, UpdateHookEntry};
use crate::plugin::{
    Component, ComponentError, ConfigurableAdapter, ConfigurableComponent, CordisError,
    DynComponentAdapter, DynPluginAdapter, ErasedComponent, FnComponent, InterceptFn, Plugin,
};
use crate::realm::RealmId;
use crate::registry::{ImplRecord, Registry};
use crate::Service;

/// 合法 unsize：`Arc<T>` → `Arc<dyn Any + Send + Sync>`。
fn erase<T: Any + Send + Sync>(t: Arc<T>) -> Arc<dyn Any + Send + Sync> {
    t
}

type LocateHit = (Option<Arc<Fiber>>, Option<Arc<ImplRecord>>);
type ResolvedDeps = (Epoch, Vec<(TypeId, Arc<ImplRecord>)>);

/// 组件身份键（基于 `Arc` 堆分配数据指针，由注册表持有强引用直至全部实例与别名释放，杜绝地址复用）。
pub(crate) fn component_key<T: ?Sized>(arc: &Arc<T>) -> usize {
    Arc::as_ptr(arc) as *const () as usize
}

struct RuntimeEntry {
    erased: Arc<dyn ErasedComponent>,
    /// 保持原始组件 `Arc` 存活以钉住身份指针。
    _pin: Arc<dyn Any + Send + Sync>,
    fibers: Vec<u64>,
}

#[derive(Clone)]
struct AliasTarget {
    key: usize,
    erased: Arc<dyn ErasedComponent>,
    pin: Arc<dyn Any + Send + Sync>,
}

/// 组件运行时注册与热替换表（对齐上游 `RegistryService`，支持 `#266` 原地 `replace` 与别名转发链）。
#[derive(Default)]
struct ComponentRegistry {
    runtimes: HashMap<usize, RuntimeEntry>,
    aliases: HashMap<usize, AliasTarget>,
}

impl ComponentRegistry {
    fn forward(
        &self,
        initial_key: usize,
        initial_erased: Arc<dyn ErasedComponent>,
        initial_pin: Arc<dyn Any + Send + Sync>,
    ) -> (usize, Arc<dyn ErasedComponent>, Arc<dyn Any + Send + Sync>) {
        let mut key = initial_key;
        let mut erased = initial_erased;
        let mut pin = initial_pin;
        let mut visited = HashSet::new();
        while visited.insert(key) {
            if let Some(next) = self.aliases.get(&key) {
                key = next.key;
                erased = Arc::clone(&next.erased);
                pin = Arc::clone(&next.pin);
            } else {
                break;
            }
        }
        (key, erased, pin)
    }

    fn mount(
        &mut self,
        raw_key: usize,
        raw_erased: Arc<dyn ErasedComponent>,
        raw_pin: Arc<dyn Any + Send + Sync>,
        fiber_id: u64,
    ) -> (usize, Arc<dyn ErasedComponent>) {
        let (key, erased, pin) = self.forward(raw_key, raw_erased, raw_pin);
        let entry = self.runtimes.entry(key).or_insert_with(|| RuntimeEntry {
            erased: Arc::clone(&erased),
            _pin: pin,
            fibers: Vec::new(),
        });
        if !entry.fibers.contains(&fiber_id) {
            entry.fibers.push(fiber_id);
        }
        (key, Arc::clone(&entry.erased))
    }

    fn detach_fiber(&mut self, runtime_key: Option<usize>, fiber_id: u64) {
        let Some(key) = runtime_key else { return };
        let should_remove = if let Some(entry) = self.runtimes.get_mut(&key) {
            entry.fibers.retain(|id| *id != fiber_id);
            entry.fibers.is_empty()
        } else {
            false
        };
        if should_remove {
            self.runtimes.remove(&key);
        }
    }

    fn has(&self, raw_key: usize) -> bool {
        let mut key = raw_key;
        let mut visited = HashSet::new();
        while visited.insert(key) {
            if let Some(next) = self.aliases.get(&key) {
                key = next.key;
            } else {
                break;
            }
        }
        self.runtimes.contains_key(&key)
    }

    fn size(&self) -> usize {
        self.runtimes.len()
    }

    /// 原地替换组件实现并记录转发别名，返回受影响的 fiber id 列表与目标擦除组件。
    fn replace(
        &mut self,
        old_raw_key: usize,
        old_erased: Arc<dyn ErasedComponent>,
        old_pin: Arc<dyn Any + Send + Sync>,
        new_key: usize,
        new_erased: Arc<dyn ErasedComponent>,
        new_pin: Arc<dyn Any + Send + Sync>,
    ) -> (Vec<u64>, usize, Arc<dyn ErasedComponent>) {
        // 新组件重新成为自身实现（解除其旧别名，支持 A -> B -> A 切回，`replace.spec.ts:234`）。
        self.aliases.remove(&new_key);

        let (source_key, _, _) = self.forward(old_raw_key, old_erased, Arc::clone(&old_pin));
        let moved_fibers = if source_key != new_key {
            let fibers = self
                .runtimes
                .remove(&source_key)
                .map(|r| r.fibers)
                .unwrap_or_default();
            self.aliases.insert(
                source_key,
                AliasTarget {
                    key: new_key,
                    erased: Arc::clone(&new_erased),
                    pin: Arc::clone(&new_pin),
                },
            );
            fibers
        } else {
            self.runtimes
                .get(&source_key)
                .map(|r| r.fibers.clone())
                .unwrap_or_default()
        };

        if moved_fibers.is_empty() {
            return (Vec::new(), new_key, new_erased);
        }

        let target_entry = self.runtimes.entry(new_key).or_insert_with(|| RuntimeEntry {
            erased: Arc::clone(&new_erased),
            _pin: new_pin,
            fibers: Vec::new(),
        });
        target_entry.erased = Arc::clone(&new_erased);
        for fid in &moved_fibers {
            if !target_entry.fibers.contains(fid) {
                target_entry.fibers.push(*fid);
            }
        }
        (moved_fibers, new_key, Arc::clone(&target_entry.erased))
    }
}

struct SharedKernel {
    store: Mutex<Registry>,
    events: Mutex<EventBus>,
    fibers: Mutex<HashMap<u64, Arc<Fiber>>>,
    comp_registry: Mutex<ComponentRegistry>,
    global_update_hooks: Mutex<Vec<UpdateHookEntry>>,
    next_id: AtomicU64,
    next_realm: AtomicU64,
    next_iso_epoch: AtomicU64,
    next_update_hook_id: AtomicU64,
    root_fiber: Arc<Fiber>,
}

struct ContextInner {
    kernel: Arc<SharedKernel>,
    /// 当前上下文所属的 fiber（根上下文为 `id = 0`；`scoped`/`isolate`/`intercept` 共享同一 fiber）。
    fiber: Arc<Fiber>,
    /// per-key 服务 realm 表（子上下文继承；未列出的 key 归 `ROOT`）。
    realms: HashMap<TypeId, RealmId>,
    /// per-key 隔离代数（每次调用 `isolate` / `isolate_at` 递增，用于精确识别隔离边界 `#209`）。
    isolation_epochs: HashMap<TypeId, u64>,
    /// 事件作用域：分发只达「本 scope + ROOT + global」的监听器。
    scope: RealmId,
    /// 上下文读取拦截器链（外层先、内层后）。
    intercepts: HashMap<TypeId, Vec<InterceptFn>>,
}

/// 上下文：效果上下文 `𝜕Γ = (𝛾, 𝜑)` 的 Rust 落地（同步至上游 `cordis v4.0.0-rc.13`）。
#[derive(Clone)]
pub struct Context {
    inner: Arc<ContextInner>,
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Context <{}>", self.inner.fiber.name())
    }
}

/// 单个 `provide_scoped` 绑定的撤回句柄（对齐上游 `const withdraw = ctx.provide(...)`）。
pub struct ProvideHandle {
    ctx: Context,
    record: Arc<ImplRecord>,
}

impl ProvideHandle {
    /// 撤回该服务绑定，触发依赖它的消费者卸载，并等待全部已提交消费者清理完毕后返回。
    pub async fn withdraw(self) {
        self.ctx.withdraw_record(&self.record).await;
    }
}

/// 异常安全的卸载清理守卫：即使消费者逆回调 panic，也确保释放 `committed` 依赖计数并唤醒等待者（`service-dispose.spec.ts:184`）。
struct UnloadGuard<'a> {
    fiber: &'a Arc<Fiber>,
    finished_cleanly: bool,
}

impl<'a> Drop for UnloadGuard<'a> {
    fn drop(&mut self) {
        if !self.finished_cleanly {
            let committed: Vec<Arc<ImplRecord>> = {
                let mut meta = self.fiber.meta.lock().unwrap();
                std::mem::take(&mut meta.committed).into_values().collect()
            };
            for rec in committed {
                rec.remove_dependent(self.fiber.id);
            }
            self.fiber.finish_in_flight();
        }
    }
}

impl Context {
    pub fn new() -> Self {
        let root_fiber = Fiber::new_root();
        let kernel = Arc::new(SharedKernel {
            store: Mutex::new(Registry::default()),
            events: Mutex::new(EventBus::default()),
            fibers: Mutex::new(HashMap::new()),
            comp_registry: Mutex::new(ComponentRegistry::default()),
            global_update_hooks: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            next_realm: AtomicU64::new(1),
            next_iso_epoch: AtomicU64::new(1),
            next_update_hook_id: AtomicU64::new(1),
            root_fiber: Arc::clone(&root_fiber),
        });
        Self {
            inner: Arc::new(ContextInner {
                kernel,
                fiber: root_fiber,
                realms: HashMap::new(),
                isolation_epochs: HashMap::new(),
                scope: RealmId::ROOT,
                intercepts: HashMap::new(),
            }),
        }
    }

    /// 当前上下文所属组件的标识名（未命名回退至父组件或 `"root"`，对齐上游 `Context <name>`）。
    pub fn name(&self) -> &'static str {
        self.inner.fiber.name()
    }

    /// 当前上下文是否仍处于活跃态（根上下文恒为 `true`；已调用 `dispose()` 的组件上下文为 `false`）。
    pub fn is_active(&self) -> bool {
        self.inner.fiber.is_active_context()
    }

    fn assert_active(&self) {
        if let Err(err) = self.inner.fiber.check_active() {
            panic!("{}", err.message);
        }
    }

    pub fn scope(&self) -> RealmId {
        self.inner.scope
    }

    /// 分配一个新的 realm id。
    pub fn alloc_realm(&self) -> RealmId {
        RealmId(self.inner.kernel.next_realm.fetch_add(1, Ordering::Relaxed))
    }

    /// key `T` 在当前上下文的 realm（未隔离 → `ROOT`）。
    pub fn realm_of<T: Service>(&self) -> RealmId {
        self.inner
            .realms
            .get(&TypeId::of::<T>())
            .copied()
            .unwrap_or(RealmId::ROOT)
    }

    /// 当前已注册的活跃组件种类数（对齐上游 `ctx.registry.size`）。
    pub fn registry_size(&self) -> usize {
        self.inner.kernel.comp_registry.lock().unwrap().size()
    }

    /// 检查某组件是否仍在注册表中有活跃实例（对齐上游 `ctx.registry.has(comp)`）。
    pub fn has_component<C: Component + ?Sized>(&self, component: &Arc<C>) -> bool {
        self.inner
            .kernel
            .comp_registry
            .lock()
            .unwrap()
            .has(component_key(component))
    }

    fn build_view(
        &self,
        fiber: Arc<Fiber>,
        realms: HashMap<TypeId, RealmId>,
        isolation_epochs: HashMap<TypeId, u64>,
        scope: RealmId,
        intercepts: HashMap<TypeId, Vec<InterceptFn>>,
    ) -> Context {
        Context {
            inner: Arc::new(ContextInner {
                kernel: Arc::clone(&self.inner.kernel),
                fiber,
                realms,
                isolation_epochs,
                scope,
                intercepts,
            }),
        }
    }

    /// 隔离 key `T` 到新 realm：`get::<T>()` / `locate::<T>()` 此后解析到独立的绑定（严格，不回退）。
    /// 共享当前 fiber 的逆栈（scope 是视图，不是生命周期边界）。
    pub fn isolate<T: Service>(&self) -> Context {
        let realm = self.alloc_realm();
        self.isolate_at::<T>(realm)
    }

    /// 隔离 key `T` 到指定 realm（即便传入相同 `realm`，也会建立新的隔离边界，阻断外层 `locate` 与注入拦截继承，`#209`）。
    pub fn isolate_at<T: Service>(&self, realm: RealmId) -> Context {
        let tid = TypeId::of::<T>();
        let mut realms = self.inner.realms.clone();
        realms.insert(tid, realm);
        let mut isolation_epochs = self.inner.isolation_epochs.clone();
        let epoch = self
            .inner
            .kernel
            .next_iso_epoch
            .fetch_add(1, Ordering::Relaxed);
        isolation_epochs.insert(tid, epoch);
        self.build_view(
            Arc::clone(&self.inner.fiber),
            realms,
            isolation_epochs,
            self.inner.scope,
            self.inner.intercepts.clone(),
        )
    }

    /// 派生一个事件作用域：事件只达「本 scope + ROOT + global」的监听器。共享逆栈。
    pub fn scoped(&self, scope: RealmId) -> Context {
        self.build_view(
            Arc::clone(&self.inner.fiber),
            self.inner.realms.clone(),
            self.inner.isolation_epochs.clone(),
            scope,
            self.inner.intercepts.clone(),
        )
    }

    /// 拦截服务读取（论文 `@@intercept`）：`get::<T>()` / `locate::<T>()` 时对解析出的值应用装饰器。
    /// 返回带该拦截的子上下文（原上下文不变）；子上下文继承，外层先注册先应用、内层后应用（内层赢）。
    pub fn intercept<T: Service>(
        &self,
        f: impl Fn(Arc<T>) -> Arc<T> + Send + Sync + 'static,
    ) -> Context {
        let erased = crate::plugin::intercept_fn::<T>(f);
        let mut intercepts = self.inner.intercepts.clone();
        intercepts
            .entry(TypeId::of::<T>())
            .or_default()
            .push(erased);
        self.build_view(
            Arc::clone(&self.inner.fiber),
            self.inner.realms.clone(),
            self.inner.isolation_epochs.clone(),
            self.inner.scope,
            intercepts,
        )
    }

    fn for_fiber(&self, fiber: Arc<Fiber>) -> Context {
        self.build_view(
            Arc::clone(&fiber),
            fiber.realms.clone(),
            fiber.isolation_epochs.clone(),
            self.inner.scope,
            self.inner.intercepts.clone(),
        )
    }

    fn from_kernel_and_fiber(kernel: Arc<SharedKernel>, fiber: Arc<Fiber>) -> Context {
        let realms = fiber.realms.clone();
        let isolation_epochs = fiber.isolation_epochs.clone();
        Context {
            inner: Arc::new(ContextInner {
                kernel,
                fiber,
                realms,
                isolation_epochs,
                scope: RealmId::ROOT,
                intercepts: HashMap::new(),
            }),
        }
    }

    // —— 共效果操作（空间可组合性）——

    /// 在当前上下文 `realm_of::<T>()` 注册服务，并把撤回与依赖者排空登记为本 fiber 的可逆效果。
    pub fn provide<T: Service>(&self, svc: Arc<T>) {
        let _ = self.provide_scoped::<T>(svc);
    }

    /// 可失败版本的 `provide`：若当前上下文已失活则返回 `Err(CordisError)`。
    pub fn try_provide<T: Service>(&self, svc: Arc<T>) -> Result<ProvideHandle, CordisError> {
        self.inner.fiber.check_active()?;
        Ok(self.provide_inner::<T>(svc))
    }

    /// 注册服务并返回可单独异步撤回的 [`ProvideHandle`]（对齐上游 `const withdraw = ctx.provide(...)`）。
    pub fn provide_scoped<T: Service>(&self, svc: Arc<T>) -> ProvideHandle {
        self.assert_active();
        self.provide_inner::<T>(svc)
    }

    fn provide_inner<T: Service>(&self, svc: Arc<T>) -> ProvideHandle {
        let realm = self.realm_of::<T>();
        let type_id = TypeId::of::<T>();
        let provider_id = self.inner.fiber.id;
        let (rec, _prev) = self.inner.kernel.store.lock().unwrap().provide_record(
            realm,
            type_id,
            erase(svc),
            provider_id,
        );
        self.inner
            .fiber
            .provisions
            .lock()
            .unwrap()
            .push(Arc::clone(&rec));

        // 登记异步逆：确保在同一 fiber 内，先于 provide 注册的底层资源 effect 在所有依赖者排空之后才逆序销毁。
        // 使用 Weak 引用杜绝 `SharedKernel -> Fiber -> Disposer -> Context -> SharedKernel` 强引用环。
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let weak_fiber = Arc::downgrade(&self.inner.fiber);
        let rec_for_dispose = Arc::clone(&rec);
        self.inner
            .fiber
            .disposers
            .lock()
            .unwrap()
            .push(Disposer::async_fut(async move {
                if let (Some(kernel), Some(fiber)) = (weak_kernel.upgrade(), weak_fiber.upgrade()) {
                    let ctx = Context::from_kernel_and_fiber(kernel, fiber);
                    ctx.withdraw_record(&rec_for_dispose).await;
                }
            }));

        // 若当前 fiber 正在同步执行 apply，推迟向外部广播直至 apply 成功完成，防止半成品绑定在失败时触发下游。
        let is_applying = self.inner.fiber.meta.lock().unwrap().applying;
        if !is_applying {
            self.notify_service_changed(realm, type_id);
        }

        ProvideHandle {
            ctx: self.clone(),
            record: rec,
        }
    }

    async fn withdraw_record(&self, rec: &Arc<ImplRecord>) {
        self.inner.kernel.store.lock().unwrap().revoke_record(rec);
        // 收集所有提交了该绑定的活跃依赖者并并发触发响应式卸载。
        let deps = rec.snapshot_dependents();
        if !deps.is_empty() {
            let futs: Vec<_> = deps
                .into_iter()
                .map(|dep_id| self.unload_fiber(dep_id, false))
                .collect();
            futures::future::join_all(futs).await;
        }
        // 即便消费者已在独立任务中先行开始卸载，仍等待其清理彻底结束。
        rec.wait_no_dependents().await;
        self.inner
            .fiber
            .provisions
            .lock()
            .unwrap()
            .retain(|r| r.uid != rec.uid);
    }

    /// 沿 fiber 祖先链解析服务访问（完全对齐上游 `reflect.ts` 的 `locate(ctx, name)`）：
    /// 1. 自身或祖先在相同 realm 提供的绑定优先返回 `(None, Some(impl))`（不附加注入拦截 `#206`）；
    /// 2. 跨越 `isolate` / `isolate_at` 边界时立即停止向上查找（`#209`）；
    /// 3. 命中声明该依赖的最近祖先 fiber 时，返回 `(Some(declaring_fiber), committed_impl)`。
    fn locate_raw(&self, type_id: TypeId) -> Option<LocateHit> {
        let target_realm = self
            .inner
            .realms
            .get(&type_id)
            .copied()
            .unwrap_or(RealmId::ROOT);
        let caller_iso_epoch = self.inner.isolation_epochs.get(&type_id).copied();

        let mut cur = Arc::clone(&self.inner.fiber);
        loop {
            // 1. 检查本 fiber 自身提供的活跃或排空中绑定（同 realm）。
            let own_prov = {
                let provs = cur.provisions.lock().unwrap();
                provs
                    .iter()
                    .rfind(|p| p.type_id == type_id && p.realm == target_realm)
                    .cloned()
            };
            if let Some(prov) = own_prov {
                return Some((None, Some(prov)));
            }

            // 2. 检查从调用点到当前 fiber 是否跨越了显式隔离边界（#209）。
            if caller_iso_epoch != cur.isolation_epoch_of(type_id) {
                return None;
            }

            // 3. 检查当前 fiber 的 committed 视图与 inject 声明。
            let (committed_impl, declared) = {
                let meta = cur.meta.lock().unwrap();
                (
                    meta.committed.get(&type_id).cloned(),
                    meta.inject.contains(&type_id),
                )
            };
            if committed_impl.is_some() || declared {
                return Some((Some(Arc::clone(&cur)), committed_impl));
            }

            // 4. 到达根 fiber 则停止，否则继续沿父 fiber 向上查找。
            let parent = cur.parent.as_ref()?;
            let next = Arc::clone(parent);
            cur = next;
        }
    }

    fn apply_intercept_chain<T: Service>(
        &self,
        base: Arc<T>,
        declaring_fiber: Option<&Arc<Fiber>>,
    ) -> Arc<T> {
        let type_id = TypeId::of::<T>();
        let inject_intercept = declaring_fiber.and_then(|f| {
            f.meta
                .lock()
                .unwrap()
                .inject_intercepts
                .get(&type_id)
                .cloned()
        });
        let ctx_intercepts = self
            .inner
            .intercepts
            .get(&type_id)
            .cloned()
            .unwrap_or_default();

        let mut value = base;
        if let Some(inj) = inject_intercept {
            let out = inj(erase(value));
            value = out
                .downcast::<T>()
                .expect("inject intercept returned wrong service type");
        }
        for entry in ctx_intercepts {
            let out = entry(erase(value));
            value = out
                .downcast::<T>()
                .expect("intercept returned wrong service type");
        }
        value
    }

    /// `get(k)`（严格活跃存储查找，对应上游 `ctx.get(name, strict = true)`）：
    /// 按 `realm_of::<T>()` 从实时存储解析（提供者进入卸载或撤回后立即返回 `None`），
    /// 并按「注入点拦截 → 外层 `ctx.intercept` → 内层 `ctx.intercept`」顺序应用装饰器。
    pub fn get<T: Service>(&self) -> Option<Arc<T>> {
        let realm = self.realm_of::<T>();
        let base = { self.inner.kernel.store.lock().unwrap().get::<T>(realm) }?;
        let declaring_fiber = self.locate_raw(TypeId::of::<T>()).and_then(|(f, _)| f);
        Some(self.apply_intercept_chain::<T>(base, declaring_fiber.as_ref()))
    }

    /// `locate(k)`（已提交依赖链查找，对应上游属性读取 `ctx.foo` / `locate(ctx, name)`）：
    /// - 在组件上下文中：只解析组件自身 `provide` 或通过自身/祖先 `inject` 提交的实例，
    ///   跨越 `isolate` 边界不会泄漏外层绑定（`#209`），且在消费者异步卸载排空期间始终可读到
    ///   该轮提交的实例（`#196`）；
    /// - 在根上下文中：直接解析当前 realm 的绑定。
    pub fn locate<T: Service>(&self) -> Option<Arc<T>> {
        if self.inner.fiber.is_root() {
            return self.get::<T>();
        }
        let (declaring_fiber, impl_rec) = self.locate_raw(TypeId::of::<T>())?;
        let rec = impl_rec?;
        let base = Arc::clone(&rec.value).downcast::<T>().ok()?;
        Some(self.apply_intercept_chain::<T>(base, declaring_fiber.as_ref()))
    }

    // —— 可逆效果（时间可组合性）——

    pub fn effect_sync(&self, f: impl FnOnce() + Send + 'static) {
        self.assert_active();
        self.inner
            .fiber
            .disposers
            .lock()
            .unwrap()
            .push(Disposer::sync(f));
    }

    pub fn try_effect_sync(&self, f: impl FnOnce() + Send + 'static) -> Result<(), CordisError> {
        self.inner.fiber.check_active()?;
        self.inner
            .fiber
            .disposers
            .lock()
            .unwrap()
            .push(Disposer::sync(f));
        Ok(())
    }

    pub fn effect_async<F>(&self, f: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.assert_active();
        self.inner
            .fiber
            .disposers
            .lock()
            .unwrap()
            .push(Disposer::async_fut(f));
    }

    pub fn try_effect_async<F>(&self, f: F) -> Result<(), CordisError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.inner.fiber.check_active()?;
        self.inner
            .fiber
            .disposers
            .lock()
            .unwrap()
            .push(Disposer::async_fut(f));
        Ok(())
    }

    /// 卸载当前上下文（根上下文卸载所有挂载的组件、绑定与逆并保持根 fiber 可复用；组件上下文等价于卸载该组件）。
    pub async fn dispose(&self) {
        if self.inner.fiber.is_root() {
            self.dispose_root().await;
        } else {
            self.dispose_fiber(self.inner.fiber.id).await;
        }
    }

    async fn dispose_root(&self) {
        let root = &self.inner.kernel.root_fiber;
        // 1. 先撤回根 fiber 直接提供的所有服务并等待消费者排空。
        let root_provs: Vec<Arc<ImplRecord>> = {
            let guard = root.provisions.lock().unwrap();
            guard.clone()
        };
        for rec in &root_provs {
            self.inner.kernel.store.lock().unwrap().revoke_record(rec);
        }
        let mut root_deps = HashSet::new();
        for rec in &root_provs {
            for dep in rec.snapshot_dependents() {
                root_deps.insert(dep);
            }
        }
        if !root_deps.is_empty() {
            let futs: Vec<_> = root_deps
                .into_iter()
                .map(|dep_id| self.unload_fiber(dep_id, false))
                .collect();
            futures::future::join_all(futs).await;
        }
        for rec in &root_provs {
            rec.wait_no_dependents().await;
        }

        // 2. LIFO 执行根上下文的所有逆（包含根上挂载的全部子组件 fiber）。
        let disposers: Vec<Disposer> = std::mem::take(&mut *root.disposers.lock().unwrap());
        for d in disposers.into_iter().rev() {
            d.run().await;
        }
        root.provisions.lock().unwrap().clear();
    }

    // —— 组件挂载、热更新与原地替换 ——

    /// 挂载组件（兼容旧 API 名称 `ctx.plugin`，内部委托给统一组件引擎）。
    pub fn plugin(&self, plugin: Arc<dyn Plugin>) -> FiberHandle {
        self.assert_active();
        let raw_key = component_key(&plugin);
        let erased: Arc<dyn ErasedComponent> = Arc::new(DynPluginAdapter(Arc::clone(&plugin)));
        let pin: Arc<dyn Any + Send + Sync> = Arc::new(plugin);
        self.mount_erased(raw_key, erased, pin, None)
    }

    /// 挂载组件（对齐上游 `v4.0.0-rc.13` `ctx.use(component)`）。
    pub fn use_component(&self, component: Arc<dyn Component>) -> FiberHandle {
        self.assert_active();
        let raw_key = component_key(&component);
        let erased: Arc<dyn ErasedComponent> =
            Arc::new(DynComponentAdapter(Arc::clone(&component)));
        let pin: Arc<dyn Any + Send + Sync> = Arc::new(component);
        self.mount_erased(raw_key, erased, pin, None)
    }

    /// 可失败版 `use_component`：若父上下文已失活则返回 `Err(CordisError)`（对齐 `#133` `INACTIVE_EFFECT`）。
    pub fn try_use_component(
        &self,
        component: Arc<dyn Component>,
    ) -> Result<FiberHandle, CordisError> {
        self.inner.fiber.check_active()?;
        let raw_key = component_key(&component);
        let erased: Arc<dyn ErasedComponent> =
            Arc::new(DynComponentAdapter(Arc::clone(&component)));
        let pin: Arc<dyn Any + Send + Sync> = Arc::new(component);
        Ok(self.mount_erased(raw_key, erased, pin, None))
    }

    /// 挂载具体类型的组件便捷方法。
    pub fn component<C: Component>(&self, component: C) -> FiberHandle {
        self.use_component(Arc::new(component))
    }

    /// 挂载带类型配置与可选校验的组件（支持 `handle.update(new_config)` 与 `ctx.on_update`）。
    pub fn use_with<C: ConfigurableComponent>(
        &self,
        component: Arc<C>,
        config: C::Config,
    ) -> FiberHandle {
        self.assert_active();
        let raw_key = component_key(&component);
        let erased: Arc<dyn ErasedComponent> =
            Arc::new(ConfigurableAdapter(Arc::clone(&component)));
        let pin: Arc<dyn Any + Send + Sync> = component;
        let raw_cfg: Arc<dyn Any + Send + Sync> = Arc::new(config);
        self.mount_erased(raw_key, erased, pin, Some(raw_cfg))
    }

    /// 声明式函数组件快捷挂载（对齐上游 `ctx.inject(deps, callback)`）。
    pub fn inject(
        &self,
        deps: impl Into<Vec<TypeId>>,
        callback: impl Fn(&Context) -> Result<(), ComponentError> + Send + Sync + 'static,
    ) -> FiberHandle {
        self.use_component(Arc::new(FnComponent {
            inject: deps.into(),
            intercepts: HashMap::new(),
            callback: Box::new(callback),
        }))
    }

    /// 带注入点拦截元数据的函数组件快捷挂载（对齐上游 `ctx.inject({ svc: meta }, callback)`，`#206`）。
    pub fn inject_with_intercepts(
        &self,
        deps: impl Into<Vec<TypeId>>,
        intercepts: HashMap<TypeId, InterceptFn>,
        callback: impl Fn(&Context) -> Result<(), ComponentError> + Send + Sync + 'static,
    ) -> FiberHandle {
        self.use_component(Arc::new(FnComponent {
            inject: deps.into(),
            intercepts,
            callback: Box::new(callback),
        }))
    }

    fn mount_erased(
        &self,
        raw_key: usize,
        raw_erased: Arc<dyn ErasedComponent>,
        raw_pin: Arc<dyn Any + Send + Sync>,
        config: Option<Arc<dyn Any + Send + Sync>>,
    ) -> FiberHandle {
        let id = self.inner.kernel.next_id.fetch_add(1, Ordering::Relaxed);
        let (runtime_key, target_erased) = self
            .inner
            .kernel
            .comp_registry
            .lock()
            .unwrap()
            .mount(raw_key, raw_erased, raw_pin, id);

        let fiber = Fiber::new_component(
            id,
            Arc::clone(&self.inner.fiber),
            self.inner.realms.clone(),
            self.inner.isolation_epochs.clone(),
            target_erased,
            Some(runtime_key),
            config,
        );
        self.inner
            .kernel
            .fibers
            .lock()
            .unwrap()
            .insert(id, Arc::clone(&fiber));

        // 将子 fiber 的永久卸载挂入父 fiber 的逆栈（嵌套组件与 root.dispose 自动级联清理；用 Weak 避免强引用环）。
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        self.inner
            .fiber
            .disposers
            .lock()
            .unwrap()
            .push(Disposer::async_fut(async move {
                if let Some(kernel) = weak_kernel.upgrade() {
                    let root_fiber = Arc::clone(&kernel.root_fiber);
                    let parent_cleanup_ctx = Context::from_kernel_and_fiber(kernel, root_fiber);
                    parent_cleanup_ctx.dispose_fiber(id).await;
                }
            }));

        let mount_ctx = self.for_fiber(Arc::clone(&fiber));
        mount_ctx.try_start(&fiber);

        FiberHandle {
            ctx: mount_ctx,
            id,
        }
    }

    /// 原地热替换已注册（或未注册）组件的全部挂载实例并建立转发别名（对齐上游 `#266` `ctx.registry.replace(old, next)`）。
    pub fn replace_component(
        &self,
        old_component: &Arc<dyn Component>,
        new_component: Arc<dyn Component>,
    ) -> Vec<FiberHandle> {
        let old_key = component_key(old_component);
        let old_erased: Arc<dyn ErasedComponent> =
            Arc::new(DynComponentAdapter(Arc::clone(old_component)));
        let old_pin: Arc<dyn Any + Send + Sync> = Arc::new(Arc::clone(old_component));

        let new_key = component_key(&new_component);
        let new_erased: Arc<dyn ErasedComponent> =
            Arc::new(DynComponentAdapter(Arc::clone(&new_component)));
        let new_pin: Arc<dyn Any + Send + Sync> = Arc::new(new_component);

        self.replace_erased_in_registry(
            old_key, old_erased, old_pin, new_key, new_erased, new_pin,
        )
    }

    /// 原地热替换带配置组件（支持用新组件 schema 重新校验已有配置，`replace.spec.ts:46`）。
    pub fn replace_configurable<C1, C2>(
        &self,
        old_component: &Arc<C1>,
        new_component: Arc<C2>,
    ) -> Vec<FiberHandle>
    where
        C1: ConfigurableComponent,
        C2: ConfigurableComponent<Config = C1::Config>,
    {
        let old_key = component_key(old_component);
        let old_erased: Arc<dyn ErasedComponent> =
            Arc::new(ConfigurableAdapter(Arc::clone(old_component)));
        let old_pin: Arc<dyn Any + Send + Sync> = Arc::clone(old_component) as _;

        let new_key = component_key(&new_component);
        let new_erased: Arc<dyn ErasedComponent> =
            Arc::new(ConfigurableAdapter(Arc::clone(&new_component)));
        let new_pin: Arc<dyn Any + Send + Sync> = new_component;

        self.replace_erased_in_registry(
            old_key, old_erased, old_pin, new_key, new_erased, new_pin,
        )
    }

    fn replace_erased_in_registry(
        &self,
        old_key: usize,
        old_erased: Arc<dyn ErasedComponent>,
        old_pin: Arc<dyn Any + Send + Sync>,
        new_key: usize,
        new_erased: Arc<dyn ErasedComponent>,
        new_pin: Arc<dyn Any + Send + Sync>,
    ) -> Vec<FiberHandle> {
        let (moved_ids, target_key, target_erased) = self
            .inner
            .kernel
            .comp_registry
            .lock()
            .unwrap()
            .replace(old_key, old_erased, old_pin, new_key, new_erased, new_pin);

        let mut handles = Vec::with_capacity(moved_ids.len());
        for fid in moved_ids {
            let Some(fiber) = self.inner.kernel.fibers.lock().unwrap().get(&fid).cloned() else {
                continue;
            };
            let fctx = self.for_fiber(Arc::clone(&fiber));
            fctx.swap_fiber_component(&fiber, Some(target_key), Arc::clone(&target_erased));
            handles.push(FiberHandle { ctx: fctx, id: fid });
        }
        handles
    }

    fn swap_fiber_component(
        &self,
        fiber: &Arc<Fiber>,
        new_runtime_key: Option<usize>,
        new_erased: Arc<dyn ErasedComponent>,
    ) {
        let new_inject = new_erased.inject();
        let new_intercepts = new_erased.inject_intercepts();
        let current_state = {
            let mut meta = fiber.meta.lock().unwrap();
            if meta.disposing_permanently || meta.state == FiberState::Disposed {
                return;
            }
            meta.component = Some(new_erased);
            meta.runtime_key = new_runtime_key;
            meta.inject = new_inject;
            meta.inject_intercepts = new_intercepts;
            meta.error = None;
            meta.revision += 1;
            meta.state
        };

        match current_state {
            FiberState::Idle | FiberState::Failed => {
                {
                    let mut meta = fiber.meta.lock().unwrap();
                    if meta.state == FiberState::Failed {
                        meta.state = FiberState::Idle;
                    }
                }
                self.try_start(fiber);
            }
            FiberState::Active | FiberState::Unloading => {
                self.schedule_or_run_reload(fiber);
            }
            FiberState::Disposed => {}
        }
    }

    fn resolve_deps(&self, fiber: &Arc<Fiber>) -> Option<ResolvedDeps> {
        let (revision, inject, disallowed) = {
            let meta = fiber.meta.lock().unwrap();
            (
                meta.revision,
                meta.inject.clone(),
                meta.disposing_permanently || meta.state == FiberState::Disposed,
            )
        };
        if disallowed {
            return None;
        }
        let store = self.inner.kernel.store.lock().unwrap();
        let mut impl_uids = Vec::with_capacity(inject.len());
        let mut records = Vec::with_capacity(inject.len());
        for t in inject {
            let rec = store.get_active_record(fiber.realm_of(t), t)?;
            impl_uids.push((t, rec.uid));
            records.push((t, rec));
        }
        Some((
            Epoch {
                revision,
                impl_uids,
            },
            records,
        ))
    }

    fn try_start(&self, fiber: &Arc<Fiber>) {
        // 失败态 fiber 绝不因依赖刷新而自动重入生命周期（#98），只能经由 update/restart/replace 清除 error 后恢复。
        {
            let meta = fiber.meta.lock().unwrap();
            if meta.error.is_some()
                || meta.state != FiberState::Idle
                || meta.disposing_permanently
                || meta.in_flight
            {
                return;
            }
        }

        let Some((epoch, records)) = self.resolve_deps(fiber) else {
            return;
        };

        let (component, raw_config) = {
            let mut meta = fiber.meta.lock().unwrap();
            if meta.error.is_some()
                || meta.state != FiberState::Idle
                || meta.disposing_permanently
                || meta.in_flight
            {
                return;
            }
            let Some(comp) = meta.component.clone() else {
                return;
            };
            meta.state = FiberState::Active;
            meta.epoch = Some(epoch);
            meta.applying = true;
            meta.committed = records
                .iter()
                .map(|(t, r)| (*t, Arc::clone(r)))
                .collect();
            (comp, meta.config.clone())
        };

        for (_, rec) in &records {
            rec.add_dependent(fiber.id);
        }

        let child_ctx = self.for_fiber(Arc::clone(fiber));
        let outcome: Result<(), CordisError> = match component.validate_config(raw_config.as_ref())
        {
            Ok(resolved_cfg) => component
                .apply_erased(&child_ctx, resolved_cfg)
                .map_err(|e| CordisError::apply_failed(e.to_string())),
            Err(validation_err) => Err(validation_err),
        };

        match outcome {
            Ok(()) => {
                fiber.meta.lock().unwrap().applying = false;
                // apply 成功完成：向等待者广播本轮 provide 的所有服务。
                let provs: Vec<(RealmId, TypeId)> = fiber
                    .provisions
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| r.active.load(Ordering::Acquire))
                    .map(|r| (r.realm, r.type_id))
                    .collect();
                for (realm, tid) in provs {
                    child_ctx.notify_service_changed(realm, tid);
                }
            }
            Err(err) => {
                eprintln!("[cordis] component apply error: {}", err.message);
                {
                    let mut meta = fiber.meta.lock().unwrap();
                    meta.applying = false;
                    meta.epoch = None;
                    meta.error = Some(err);
                    meta.state = FiberState::Failed;
                }
                self.rollback_failed_apply(fiber);
            }
        }
    }

    /// `apply` 半途失败时的完整回滚（修复旧版只撤 `provides` 而泄漏 `effect_*`/`on_*_scoped` 的缺陷，`#98`/`#133`）。
    fn rollback_failed_apply(&self, fiber: &Arc<Fiber>) {
        let provs: Vec<Arc<ImplRecord>> = std::mem::take(&mut *fiber.provisions.lock().unwrap());
        {
            let mut store = self.inner.kernel.store.lock().unwrap();
            for rec in &provs {
                store.revoke_record(rec);
            }
        }
        fiber.update_hooks.lock().unwrap().clear();

        let disposers: Vec<Disposer> = std::mem::take(&mut *fiber.disposers.lock().unwrap());
        let mut async_rollbacks = Vec::new();
        for d in disposers.into_iter().rev() {
            match d {
                Disposer::Sync(f) => f(),
                Disposer::Async(fut) => async_rollbacks.push(fut),
            }
        }

        if async_rollbacks.is_empty() {
            let committed: Vec<Arc<ImplRecord>> = {
                let mut meta = fiber.meta.lock().unwrap();
                std::mem::take(&mut meta.committed).into_values().collect()
            };
            for rec in committed {
                rec.remove_dependent(fiber.id);
            }
        } else {
            // 若失败前注册了异步逆，则在首次 poll 时尝试立即完成，若仍挂起则存入 deferred_task 由 await_ready/dispose 驱动，
            // 且在此期间保持 committed 引用直至异步逆完成（service-dispose.spec.ts:156）。
            let weak_fiber = Arc::downgrade(fiber);
            let mut task: BoxFuture<'static, ()> = Box::pin(async move {
                for fut in async_rollbacks {
                    fut.await;
                }
                if let Some(fiber_clone) = weak_fiber.upgrade() {
                    let committed: Vec<Arc<ImplRecord>> = {
                        let mut meta = fiber_clone.meta.lock().unwrap();
                        std::mem::take(&mut meta.committed).into_values().collect()
                    };
                    for rec in committed {
                        rec.remove_dependent(fiber_clone.id);
                    }
                }
            });
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            if task.as_mut().poll(&mut cx).is_pending() {
                fiber.meta.lock().unwrap().deferred_task = Some(task);
            }
        }
    }

    /// 唤醒受 `(realm, type_id)` 就绪影响的等待态（`Idle`）fiber。
    /// 已活跃组件的卸载由提供者撤回绑定时（`withdraw_record` / `unload_fiber`）沿 `dependents` 精确级联触发，
    /// 从而在 `root` 上覆盖更新配置服务时不会误卸载同域已运行的组件。
    fn notify_service_changed(&self, realm: RealmId, type_id: TypeId) {
        let fibers: Vec<Arc<Fiber>> = self
            .inner
            .kernel
            .fibers
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for fiber in fibers {
            let should_try_start = {
                let meta = fiber.meta.lock().unwrap();
                meta.state == FiberState::Idle
                    && meta.error.is_none()
                    && !meta.disposing_permanently
                    && meta.inject.contains(&type_id)
                    && fiber.realm_of(type_id) == realm
            };
            if should_try_start {
                let fctx = self.for_fiber(Arc::clone(&fiber));
                fctx.try_start(&fiber);
            }
        }
    }

    /// 当同步入口（如 `replace_component` 或覆盖 `provide`）需要重载一个已活跃 fiber 时：
    /// 若整棵级联卸载子树全为同步逆，则当场同步完成卸载与重装；若含异步逆，则封装进 `deferred_task` 由 `await_ready`/`dispose` 驱动。
    fn schedule_or_run_reload(&self, fiber: &Arc<Fiber>) {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let weak_fiber = Arc::downgrade(fiber);
        let fid = fiber.id;
        let mut fut: BoxFuture<'static, ()> = Box::pin(async move {
            if let (Some(kernel), Some(f)) = (weak_kernel.upgrade(), weak_fiber.upgrade()) {
                let ctx = Context::from_kernel_and_fiber(kernel, f);
                ctx.unload_fiber(fid, false).await;
            }
        });
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        if fut.as_mut().poll(&mut cx).is_pending() {
            fiber.meta.lock().unwrap().deferred_task = Some(fut);
        }
    }

    /// 永久卸载指定 fiber，并响应式级联卸载因之缺失依赖的下游组件。
    pub async fn dispose_fiber(&self, id: u64) {
        self.unload_fiber(id, true).await;
    }

    /// 统一卸载引擎：
    /// - `explicit = true`（调用 `dispose()`）：目标 fiber 最终收敛至 `FiberState::Disposed` 并从注册表移除；
    /// - `explicit = false`（因依赖撤回/替换/重启触发）：目标 fiber 清理后收敛回 `FiberState::Idle`（若新依赖就绪则立即重装为 `Active`）。
    fn unload_fiber(&self, id: u64, explicit: bool) -> BoxFuture<'static, ()> {
        let ctx = self.clone();
        Box::pin(async move {
            let fiber = ctx.inner.kernel.fibers.lock().unwrap().get(&id).cloned();
            let Some(fiber) = fiber else { return };

            if explicit {
                let runtime_key = {
                    let mut meta = fiber.meta.lock().unwrap();
                    if meta.state == FiberState::Disposed {
                        return;
                    }
                    meta.disposing_permanently = true;
                    meta.runtime_key.take()
                };
                ctx.inner
                    .kernel
                    .comp_registry
                    .lock()
                    .unwrap()
                    .detach_fiber(runtime_key, id);
            }

            // 若已有异步转换正在飞行中，等待其结束后复核最终状态。
            loop {
                let (in_flight, has_deferred, state) = {
                    let meta = fiber.meta.lock().unwrap();
                    (meta.in_flight, meta.deferred_task.is_some(), meta.state)
                };
                if in_flight || has_deferred {
                    fiber.wait_idle().await;
                    continue;
                }
                match state {
                    FiberState::Disposed => return,
                    FiberState::Idle | FiberState::Failed => {
                        if explicit {
                            fiber.meta.lock().unwrap().state = FiberState::Disposed;
                            ctx.inner.kernel.fibers.lock().unwrap().remove(&id);
                        }
                        return;
                    }
                    FiberState::Active => {
                        let mut meta = fiber.meta.lock().unwrap();
                        if meta.in_flight || meta.state != FiberState::Active {
                            continue;
                        }
                        meta.in_flight = true;
                        meta.state = FiberState::Unloading;
                        meta.epoch = None;
                        break;
                    }
                    FiberState::Unloading => {
                        fiber.wait_idle().await;
                    }
                }
            }

            let mut guard = UnloadGuard {
                fiber: &fiber,
                finished_cleanly: false,
            };

            // 1. 提前从实时存储撤下本 fiber 提供的绑定（strict `ctx.get` 立即感知缺失，满足 R1.5）。
            //    注意：保留在 `fiber.provisions` 中直至本 fiber 自身逆也执行完毕，使组合子组件仍可通过 `locate` 读取继承服务。
            let own_provisions: Vec<Arc<ImplRecord>> =
                fiber.provisions.lock().unwrap().clone();
            {
                let mut store = ctx.inner.kernel.store.lock().unwrap();
                for rec in &own_provisions {
                    store.revoke_record(rec);
                }
            }

            // 2. 收集所有因本 fiber 撤回绑定而受影响的下游依赖者，并并发触发响应式卸载。
            let mut dep_ids = HashSet::new();
            for rec in &own_provisions {
                for dep in rec.snapshot_dependents() {
                    if dep != id {
                        dep_ids.insert(dep);
                    }
                }
            }
            if !dep_ids.is_empty() {
                let futs: Vec<_> = dep_ids
                    .into_iter()
                    .map(|dep_id| ctx.unload_fiber(dep_id, false))
                    .collect();
                futures::future::join_all(futs).await;
            }

            // 3. 等待所有当前与历史已撤回绑定的已提交消费者彻底完成异步清理（含独立调用 `consumer.dispose()` 的消费者，`#196`）。
            for rec in &own_provisions {
                rec.wait_no_dependents().await;
            }

            // 4. 清理本轮挂载的 fiber 局部 update 钩子，随后按 LIFO 顺序执行本 fiber 的所有逆。
            fiber.update_hooks.lock().unwrap().clear();
            let disposers: Vec<Disposer> =
                std::mem::take(&mut *fiber.disposers.lock().unwrap());
            for d in disposers.into_iter().rev() {
                d.run().await;
            }

            // 5. 自身及子组件逆全部完成后，清空 own_provisions 并释放本 fiber 对上游服务的 committed 引用。
            fiber.provisions.lock().unwrap().clear();
            let committed: Vec<Arc<ImplRecord>> = {
                let mut meta = fiber.meta.lock().unwrap();
                std::mem::take(&mut meta.committed).into_values().collect()
            };
            for rec in committed {
                rec.remove_dependent(id);
            }

            // 6. 结算最终状态：永久卸载收敛为 Disposed，响应式卸载收敛为 Idle 并在新依赖可用时立即重装。
            let permanently_disposed = {
                let mut meta = fiber.meta.lock().unwrap();
                meta.in_flight = false;
                if meta.disposing_permanently {
                    meta.state = FiberState::Disposed;
                    true
                } else {
                    meta.state = FiberState::Idle;
                    false
                }
            };
            guard.finished_cleanly = true;

            let waiters = std::mem::take(&mut fiber.meta.lock().unwrap().idle_waiters);
            for tx in waiters {
                let _ = tx.send(());
            }

            if permanently_disposed {
                ctx.inner.kernel.fibers.lock().unwrap().remove(&id);
            } else {
                let fctx = ctx.for_fiber(Arc::clone(&fiber));
                fctx.try_start(&fiber);
            }
        })
    }

    // —— 配置热更新钩子（`#56` `internal/update`） ——

    /// 注册组件配置热更新钩子（对齐上游 `ctx.on('internal/update', (config, next) => ...)`）：
    /// - 默认（`options.global = false`）绑定在当前 fiber 上，随每次重载自动清理，绝不跨重载累积（`#56`）；
    /// - 调用 `next()` 继续委托下游直至触发组件 `restart()`；不调用 `next()` 则拦截本次更新、保持组件热运行不重启。
    pub fn on_update<Cfg: Clone + Send + Sync + 'static>(
        &self,
        f: impl Fn(Cfg, WaterfallNext<()>) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> Disposer {
        self.on_update_with::<Cfg>(EventOptions::default(), f)
    }

    pub fn on_update_with<Cfg: Clone + Send + Sync + 'static>(
        &self,
        options: EventOptions,
        f: impl Fn(Cfg, WaterfallNext<()>) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> Disposer {
        self.assert_active();
        let hook_id = self
            .inner
            .kernel
            .next_update_hook_id
            .fetch_add(1, Ordering::Relaxed);
        let removed = Arc::new(AtomicBool::new(false));
        let f = Arc::new(f);
        let erased: ErasedUpdateFn = Arc::new(move |cfg_any, next| {
            if let Some(cfg) = cfg_any.downcast_ref::<Cfg>().cloned() {
                f(cfg, next)
            } else {
                next()
            }
        });
        let entry = UpdateHookEntry {
            id: hook_id,
            removed: Arc::clone(&removed),
            callback: erased,
        };

        if options.global {
            let mut list = self.inner.kernel.global_update_hooks.lock().unwrap();
            if options.prepend {
                list.insert(0, entry);
            } else {
                list.push(entry);
            }
            drop(list);
            let weak_kernel = Arc::downgrade(&self.inner.kernel);
            let removed_cl = Arc::clone(&removed);
            let remove_fn = move || {
                removed_cl.store(true, Ordering::Release);
                if let Some(kernel) = weak_kernel.upgrade() {
                    kernel
                        .global_update_hooks
                        .lock()
                        .unwrap()
                        .retain(|h| h.id != hook_id);
                }
            };
            let remove_arc = Arc::new(Mutex::new(Some(remove_fn)));
            let r1 = Arc::clone(&remove_arc);
            self.effect_sync(move || {
                if let Some(f) = r1.lock().unwrap().take() {
                    f();
                }
            });
            Disposer::sync(move || {
                if let Some(f) = remove_arc.lock().unwrap().take() {
                    f();
                }
            })
        } else {
            let mut list = self.inner.fiber.update_hooks.lock().unwrap();
            if options.prepend {
                list.insert(0, entry);
            } else {
                list.push(entry);
            }
            drop(list);
            let weak_fiber = Arc::downgrade(&self.inner.fiber);
            let removed_cl = Arc::clone(&removed);
            let remove_fn = move || {
                removed_cl.store(true, Ordering::Release);
                if let Some(fiber) = weak_fiber.upgrade() {
                    fiber
                        .update_hooks
                        .lock()
                        .unwrap()
                        .retain(|h| h.id != hook_id);
                }
            };
            let remove_arc = Arc::new(Mutex::new(Some(remove_fn)));
            let r1 = Arc::clone(&remove_arc);
            self.effect_sync(move || {
                if let Some(f) = r1.lock().unwrap().take() {
                    f();
                }
            });
            Disposer::sync(move || {
                if let Some(f) = remove_arc.lock().unwrap().take() {
                    f();
                }
            })
        }
    }

    // —— 事件（scope 作用域分发：本 scope + ROOT + global；分发期移除即时生效 `#242`） ——

    /// 检查指定事件类型在事件总线中是否仍有非空桶（用于测试 `#51` 空桶清理）。
    pub fn has_event_bucket<E: 'static>(&self) -> bool {
        self.inner.kernel.events.lock().unwrap().has_bucket::<E>()
    }

    fn effect_disposer(&self, d: Disposer) {
        match d {
            Disposer::Sync(f) => self.effect_sync(f),
            Disposer::Async(f) => self.effect_async(f),
        }
    }

    pub fn on_emit<E: EmitEvent>(&self, f: impl Fn(E::Args) + Send + Sync + 'static) -> Disposer {
        self.on_emit_with::<E>(EventOptions::default(), f)
    }

    pub fn on_emit_with<E: EmitEvent>(
        &self,
        options: EventOptions,
        f: impl Fn(E::Args) + Send + Sync + 'static,
    ) -> Disposer {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let scope = self.inner.scope;
        let removed = Arc::new(AtomicBool::new(false));
        let id = self
            .inner
            .kernel
            .events
            .lock()
            .unwrap()
            .on_emit_with::<E>(scope, options, Arc::clone(&removed), f);
        Disposer::sync(move || {
            removed.store(true, Ordering::Release);
            if let Some(kernel) = weak_kernel.upgrade() {
                kernel.events.lock().unwrap().remove_emit::<E>(id);
            }
        })
    }

    pub fn on_emit_once<E: EmitEvent>(
        &self,
        f: impl Fn(E::Args) + Send + Sync + 'static,
    ) -> Disposer {
        self.on_emit_once_with::<E>(EventOptions::default(), f)
    }

    pub fn on_emit_once_with<E: EmitEvent>(
        &self,
        options: EventOptions,
        f: impl Fn(E::Args) + Send + Sync + 'static,
    ) -> Disposer {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let weak_in_cb = Arc::downgrade(&self.inner.kernel);
        let scope = self.inner.scope;
        let removed = Arc::new(AtomicBool::new(false));
        let removed_in_cb = Arc::clone(&removed);
        let id_slot = Arc::new(AtomicU64::new(u64::MAX));
        let id_in_cb = Arc::clone(&id_slot);
        let id = self.inner.kernel.events.lock().unwrap().on_emit_with::<E>(
            scope,
            options,
            Arc::clone(&removed),
            move |args| {
                if removed_in_cb.swap(true, Ordering::AcqRel) {
                    return;
                }
                let hid = id_in_cb.load(Ordering::Acquire);
                if hid != u64::MAX {
                    if let Some(kernel_in_cb) = weak_in_cb.upgrade() {
                        kernel_in_cb.events.lock().unwrap().remove_emit::<E>(hid);
                    }
                }
                f(args);
            },
        );
        id_slot.store(id, Ordering::Release);
        if removed.load(Ordering::Acquire) {
            self.inner.kernel.events.lock().unwrap().remove_emit::<E>(id);
        }
        Disposer::sync(move || {
            removed.store(true, Ordering::Release);
            if let Some(kernel) = weak_kernel.upgrade() {
                kernel.events.lock().unwrap().remove_emit::<E>(id);
            }
        })
    }

    /// fiber 作用域版 `on_emit`：监听器随当前 fiber 卸载自动移除。
    pub fn on_emit_scoped<E: EmitEvent>(&self, f: impl Fn(E::Args) + Send + Sync + 'static) {
        self.assert_active();
        let d = self.on_emit::<E>(f);
        self.effect_disposer(d);
    }

    pub fn try_on_emit_scoped<E: EmitEvent>(
        &self,
        f: impl Fn(E::Args) + Send + Sync + 'static,
    ) -> Result<(), CordisError> {
        self.inner.fiber.check_active()?;
        let d = self.on_emit::<E>(f);
        self.effect_disposer(d);
        Ok(())
    }

    pub fn on_emit_once_scoped<E: EmitEvent>(&self, f: impl Fn(E::Args) + Send + Sync + 'static) {
        self.assert_active();
        let d = self.on_emit_once::<E>(f);
        self.effect_disposer(d);
    }

    pub fn emit<E: EmitEvent>(&self, args: E::Args) {
        let scope = self.inner.scope;
        let listeners = self
            .inner
            .kernel
            .events
            .lock()
            .unwrap()
            .emit_listeners::<E>(scope);
        for (removed, l) in listeners {
            if !removed.load(Ordering::Acquire) {
                l(args.clone());
            }
        }
    }

    pub fn on_parallel<E: ParallelEvent>(
        &self,
        f: impl Fn(E::Args) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> Disposer {
        self.on_parallel_with::<E>(EventOptions::default(), f)
    }

    pub fn on_parallel_with<E: ParallelEvent>(
        &self,
        options: EventOptions,
        f: impl Fn(E::Args) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> Disposer {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let scope = self.inner.scope;
        let removed = Arc::new(AtomicBool::new(false));
        let id = self.inner.kernel.events.lock().unwrap().on_parallel_with::<E>(
            scope,
            options,
            Arc::clone(&removed),
            f,
        );
        Disposer::sync(move || {
            removed.store(true, Ordering::Release);
            if let Some(kernel) = weak_kernel.upgrade() {
                kernel.events.lock().unwrap().remove_parallel::<E>(id);
            }
        })
    }

    pub fn on_parallel_once<E: ParallelEvent>(
        &self,
        f: impl Fn(E::Args) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> Disposer {
        self.on_parallel_once_with::<E>(EventOptions::default(), f)
    }

    pub fn on_parallel_once_with<E: ParallelEvent>(
        &self,
        options: EventOptions,
        f: impl Fn(E::Args) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> Disposer {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let weak_in_cb = Arc::downgrade(&self.inner.kernel);
        let scope = self.inner.scope;
        let removed = Arc::new(AtomicBool::new(false));
        let removed_in_cb = Arc::clone(&removed);
        let id_slot = Arc::new(AtomicU64::new(u64::MAX));
        let id_in_cb = Arc::clone(&id_slot);
        let id = self.inner.kernel.events.lock().unwrap().on_parallel_with::<E>(
            scope,
            options,
            Arc::clone(&removed),
            move |args| {
                if removed_in_cb.swap(true, Ordering::AcqRel) {
                    return Box::pin(async {});
                }
                let hid = id_in_cb.load(Ordering::Acquire);
                if hid != u64::MAX {
                    if let Some(kernel_in_cb) = weak_in_cb.upgrade() {
                        kernel_in_cb
                            .events
                            .lock()
                            .unwrap()
                            .remove_parallel::<E>(hid);
                    }
                }
                f(args)
            },
        );
        id_slot.store(id, Ordering::Release);
        if removed.load(Ordering::Acquire) {
            self.inner.kernel.events.lock().unwrap().remove_parallel::<E>(id);
        }
        Disposer::sync(move || {
            removed.store(true, Ordering::Release);
            if let Some(kernel) = weak_kernel.upgrade() {
                kernel.events.lock().unwrap().remove_parallel::<E>(id);
            }
        })
    }

    pub async fn parallel<E: ParallelEvent>(&self, args: E::Args) {
        let scope = self.inner.scope;
        let listeners = self
            .inner
            .kernel
            .events
            .lock()
            .unwrap()
            .parallel_listeners::<E>(scope);
        let futs: Vec<_> = listeners
            .into_iter()
            .map(|(removed, l)| {
                let arg = args.clone();
                async move {
                    if !removed.load(Ordering::Acquire) {
                        l(arg).await;
                    }
                }
            })
            .collect();
        futures::future::join_all(futs).await;
    }

    /// fiber 作用域版 `on_parallel`：监听器随当前 fiber 卸载自动移除。
    pub fn on_parallel_scoped<E: ParallelEvent>(
        &self,
        f: impl Fn(E::Args) -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) {
        self.assert_active();
        let d = self.on_parallel::<E>(f);
        self.effect_disposer(d);
    }

    pub fn on_serial<E: SerialEvent>(
        &self,
        f: impl Fn(E::Args) -> BoxFuture<'static, Option<E::Ret>> + Send + Sync + 'static,
    ) -> Disposer {
        self.on_serial_with::<E>(EventOptions::default(), f)
    }

    pub fn on_serial_with<E: SerialEvent>(
        &self,
        options: EventOptions,
        f: impl Fn(E::Args) -> BoxFuture<'static, Option<E::Ret>> + Send + Sync + 'static,
    ) -> Disposer {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let scope = self.inner.scope;
        let removed = Arc::new(AtomicBool::new(false));
        let id = self.inner.kernel.events.lock().unwrap().on_serial_with::<E>(
            scope,
            options,
            Arc::clone(&removed),
            f,
        );
        Disposer::sync(move || {
            removed.store(true, Ordering::Release);
            if let Some(kernel) = weak_kernel.upgrade() {
                kernel.events.lock().unwrap().remove_serial::<E>(id);
            }
        })
    }

    pub fn on_serial_once<E: SerialEvent>(
        &self,
        f: impl Fn(E::Args) -> BoxFuture<'static, Option<E::Ret>> + Send + Sync + 'static,
    ) -> Disposer {
        self.on_serial_once_with::<E>(EventOptions::default(), f)
    }

    pub fn on_serial_once_with<E: SerialEvent>(
        &self,
        options: EventOptions,
        f: impl Fn(E::Args) -> BoxFuture<'static, Option<E::Ret>> + Send + Sync + 'static,
    ) -> Disposer {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let weak_in_cb = Arc::downgrade(&self.inner.kernel);
        let scope = self.inner.scope;
        let removed = Arc::new(AtomicBool::new(false));
        let removed_in_cb = Arc::clone(&removed);
        let id_slot = Arc::new(AtomicU64::new(u64::MAX));
        let id_in_cb = Arc::clone(&id_slot);
        let id = self.inner.kernel.events.lock().unwrap().on_serial_with::<E>(
            scope,
            options,
            Arc::clone(&removed),
            move |args| {
                if removed_in_cb.swap(true, Ordering::AcqRel) {
                    return Box::pin(async { None });
                }
                let hid = id_in_cb.load(Ordering::Acquire);
                if hid != u64::MAX {
                    if let Some(kernel_in_cb) = weak_in_cb.upgrade() {
                        kernel_in_cb.events.lock().unwrap().remove_serial::<E>(hid);
                    }
                }
                f(args)
            },
        );
        id_slot.store(id, Ordering::Release);
        if removed.load(Ordering::Acquire) {
            self.inner.kernel.events.lock().unwrap().remove_serial::<E>(id);
        }
        Disposer::sync(move || {
            removed.store(true, Ordering::Release);
            if let Some(kernel) = weak_kernel.upgrade() {
                kernel.events.lock().unwrap().remove_serial::<E>(id);
            }
        })
    }

    pub async fn serial<E: SerialEvent>(&self, args: E::Args) -> Option<E::Ret> {
        let scope = self.inner.scope;
        let listeners = self
            .inner
            .kernel
            .events
            .lock()
            .unwrap()
            .serial_listeners::<E>(scope);
        for (removed, l) in listeners {
            if removed.load(Ordering::Acquire) {
                continue;
            }
            if let Some(r) = l(args.clone()).await {
                return Some(r);
            }
        }
        None
    }

    /// fiber 作用域版 `on_serial`：监听器随当前 fiber 卸载自动移除。
    pub fn on_serial_scoped<E: SerialEvent>(
        &self,
        f: impl Fn(E::Args) -> BoxFuture<'static, Option<E::Ret>> + Send + Sync + 'static,
    ) {
        self.assert_active();
        let d = self.on_serial::<E>(f);
        self.effect_disposer(d);
    }

    pub fn on_bail<E: BailEvent>(
        &self,
        f: impl Fn(E::Args) -> Option<E::Ret> + Send + Sync + 'static,
    ) -> Disposer {
        self.on_bail_with::<E>(EventOptions::default(), f)
    }

    pub fn on_bail_with<E: BailEvent>(
        &self,
        options: EventOptions,
        f: impl Fn(E::Args) -> Option<E::Ret> + Send + Sync + 'static,
    ) -> Disposer {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let scope = self.inner.scope;
        let removed = Arc::new(AtomicBool::new(false));
        let id = self.inner.kernel.events.lock().unwrap().on_bail_with::<E>(
            scope,
            options,
            Arc::clone(&removed),
            f,
        );
        Disposer::sync(move || {
            removed.store(true, Ordering::Release);
            if let Some(kernel) = weak_kernel.upgrade() {
                kernel.events.lock().unwrap().remove_bail::<E>(id);
            }
        })
    }

    pub fn on_bail_once<E: BailEvent>(
        &self,
        f: impl Fn(E::Args) -> Option<E::Ret> + Send + Sync + 'static,
    ) -> Disposer {
        self.on_bail_once_with::<E>(EventOptions::default(), f)
    }

    pub fn on_bail_once_with<E: BailEvent>(
        &self,
        options: EventOptions,
        f: impl Fn(E::Args) -> Option<E::Ret> + Send + Sync + 'static,
    ) -> Disposer {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let weak_in_cb = Arc::downgrade(&self.inner.kernel);
        let scope = self.inner.scope;
        let removed = Arc::new(AtomicBool::new(false));
        let removed_in_cb = Arc::clone(&removed);
        let id_slot = Arc::new(AtomicU64::new(u64::MAX));
        let id_in_cb = Arc::clone(&id_slot);
        let id = self.inner.kernel.events.lock().unwrap().on_bail_with::<E>(
            scope,
            options,
            Arc::clone(&removed),
            move |args| {
                if removed_in_cb.swap(true, Ordering::AcqRel) {
                    return None;
                }
                let hid = id_in_cb.load(Ordering::Acquire);
                if hid != u64::MAX {
                    if let Some(kernel_in_cb) = weak_in_cb.upgrade() {
                        kernel_in_cb.events.lock().unwrap().remove_bail::<E>(hid);
                    }
                }
                f(args)
            },
        );
        id_slot.store(id, Ordering::Release);
        if removed.load(Ordering::Acquire) {
            self.inner.kernel.events.lock().unwrap().remove_bail::<E>(id);
        }
        Disposer::sync(move || {
            removed.store(true, Ordering::Release);
            if let Some(kernel) = weak_kernel.upgrade() {
                kernel.events.lock().unwrap().remove_bail::<E>(id);
            }
        })
    }

    pub fn bail<E: BailEvent>(&self, args: E::Args) -> Option<E::Ret> {
        let scope = self.inner.scope;
        let listeners = self
            .inner
            .kernel
            .events
            .lock()
            .unwrap()
            .bail_listeners::<E>(scope);
        for (removed, l) in listeners {
            if removed.load(Ordering::Acquire) {
                continue;
            }
            if let Some(r) = l(args.clone()) {
                return Some(r);
            }
        }
        None
    }

    /// fiber 作用域版 `on_bail`：监听器随当前 fiber 卸载自动移除。
    pub fn on_bail_scoped<E: BailEvent>(
        &self,
        f: impl Fn(E::Args) -> Option<E::Ret> + Send + Sync + 'static,
    ) {
        self.assert_active();
        let d = self.on_bail::<E>(f);
        self.effect_disposer(d);
    }

    pub fn on_waterfall<E: WaterfallEvent>(
        &self,
        f: impl Fn(E::Args, WaterfallNext<E::Ret>) -> BoxFuture<'static, E::Ret>
            + Send
            + Sync
            + 'static,
    ) -> Disposer {
        self.on_waterfall_with::<E>(EventOptions::default(), f)
    }

    pub fn on_waterfall_with<E: WaterfallEvent>(
        &self,
        options: EventOptions,
        f: impl Fn(E::Args, WaterfallNext<E::Ret>) -> BoxFuture<'static, E::Ret>
            + Send
            + Sync
            + 'static,
    ) -> Disposer {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let scope = self.inner.scope;
        let removed = Arc::new(AtomicBool::new(false));
        let id = self.inner.kernel.events.lock().unwrap().on_waterfall_with::<E>(
            scope,
            options,
            Arc::clone(&removed),
            f,
        );
        Disposer::sync(move || {
            removed.store(true, Ordering::Release);
            if let Some(kernel) = weak_kernel.upgrade() {
                kernel.events.lock().unwrap().remove_waterfall::<E>(id);
            }
        })
    }

    pub fn on_waterfall_once<E: WaterfallEvent>(
        &self,
        f: impl Fn(E::Args, WaterfallNext<E::Ret>) -> BoxFuture<'static, E::Ret>
            + Send
            + Sync
            + 'static,
    ) -> Disposer {
        self.on_waterfall_once_with::<E>(EventOptions::default(), f)
    }

    pub fn on_waterfall_once_with<E: WaterfallEvent>(
        &self,
        options: EventOptions,
        f: impl Fn(E::Args, WaterfallNext<E::Ret>) -> BoxFuture<'static, E::Ret>
            + Send
            + Sync
            + 'static,
    ) -> Disposer {
        let weak_kernel = Arc::downgrade(&self.inner.kernel);
        let weak_in_cb = Arc::downgrade(&self.inner.kernel);
        let scope = self.inner.scope;
        let removed = Arc::new(AtomicBool::new(false));
        let removed_in_cb = Arc::clone(&removed);
        let id_slot = Arc::new(AtomicU64::new(u64::MAX));
        let id_in_cb = Arc::clone(&id_slot);
        let id = self.inner.kernel.events.lock().unwrap().on_waterfall_with::<E>(
            scope,
            options,
            Arc::clone(&removed),
            move |args, next| {
                if removed_in_cb.swap(true, Ordering::AcqRel) {
                    return next();
                }
                let hid = id_in_cb.load(Ordering::Acquire);
                if hid != u64::MAX {
                    if let Some(kernel_in_cb) = weak_in_cb.upgrade() {
                        kernel_in_cb
                            .events
                            .lock()
                            .unwrap()
                            .remove_waterfall::<E>(hid);
                    }
                }
                f(args, next)
            },
        );
        id_slot.store(id, Ordering::Release);
        if removed.load(Ordering::Acquire) {
            self.inner
                .kernel
                .events
                .lock()
                .unwrap()
                .remove_waterfall::<E>(id);
        }
        Disposer::sync(move || {
            removed.store(true, Ordering::Release);
            if let Some(kernel) = weak_kernel.upgrade() {
                kernel.events.lock().unwrap().remove_waterfall::<E>(id);
            }
        })
    }

    /// fiber 作用域版 `on_waterfall`：监听器随当前 fiber 卸载自动移除。
    pub fn on_waterfall_scoped<E: WaterfallEvent>(
        &self,
        f: impl Fn(E::Args, WaterfallNext<E::Ret>) -> BoxFuture<'static, E::Ret>
            + Send
            + Sync
            + 'static,
    ) {
        self.assert_active();
        let d = self.on_waterfall::<E>(f);
        self.effect_disposer(d);
    }

    pub fn waterfall<E, I>(&self, args: E::Args, inner: I) -> BoxFuture<'static, E::Ret>
    where
        E: WaterfallEvent,
        I: FnOnce(E::Args) -> BoxFuture<'static, E::Ret> + Send + 'static,
    {
        let scope = self.inner.scope;
        let listeners = Arc::new({
            self.inner
                .kernel
                .events
                .lock()
                .unwrap()
                .waterfall_listeners::<E>(scope)
        });

        type SnapVec<E> = Vec<(
            Arc<AtomicBool>,
            Arc<
                dyn Fn(
                        <E as WaterfallEvent>::Args,
                        WaterfallNext<<E as WaterfallEvent>::Ret>,
                    ) -> BoxFuture<'static, <E as WaterfallEvent>::Ret>
                    + Send
                    + Sync,
            >,
        )>;

        fn run<E, I>(
            listeners: Arc<SnapVec<E>>,
            mut idx: usize,
            args: E::Args,
            inner: I,
        ) -> BoxFuture<'static, E::Ret>
        where
            E: WaterfallEvent,
            I: FnOnce(E::Args) -> BoxFuture<'static, E::Ret> + Send + 'static,
        {
            while idx < listeners.len() && listeners[idx].0.load(Ordering::Acquire) {
                idx += 1;
            }
            if idx < listeners.len() {
                let l = Arc::clone(&listeners[idx].1);
                let args_next = args.clone();
                let next: WaterfallNext<E::Ret> =
                    Box::new(move || run::<E, I>(listeners, idx + 1, args_next, inner));
                l(args, next)
            } else {
                inner(args)
            }
        }

        run::<E, I>(listeners, 0, args, inner)
    }
}

/// 组件的运行时句柄（对齐上游 `Fiber` 句柄）。
#[derive(Clone)]
pub struct FiberHandle {
    ctx: Context,
    id: u64,
}

impl FiberHandle {
    /// 组件唯一 ID（若已卸载仍返回分配时的数值标识，状态见 [`Self::state`]）。
    pub fn id(&self) -> u64 {
        self.id
    }

    /// 组件内部上下文引用。
    pub fn context(&self) -> &Context {
        &self.ctx
    }

    fn lookup_fiber(&self) -> Option<Arc<Fiber>> {
        self.ctx
            .inner
            .kernel
            .fibers
            .lock()
            .unwrap()
            .get(&self.id)
            .cloned()
    }

    /// 永久卸载该组件（LIFO 执行其逆），并级联卸载因之缺失依赖的组件。
    pub async fn dispose(&self) {
        self.ctx.dispose_fiber(self.id).await;
    }

    /// 当前生命周期状态（`Idle` / `Active` / `Failed` / `Unloading` / `Disposed`）。
    pub fn state(&self) -> FiberState {
        match self.lookup_fiber() {
            Some(f) => f.state(),
            None => FiberState::Disposed,
        }
    }

    /// 最近一次失败的结构化错误（若组件处于 `FiberState::Failed`）。
    pub fn error(&self) -> Option<CordisError> {
        self.lookup_fiber().and_then(|f| f.error())
    }

    /// 读取当前保存的组件源配置副本（对应上游 `fiber.config`）。
    pub fn config<Cfg: Clone + Send + Sync + 'static>(&self) -> Option<Cfg> {
        let fiber = self.lookup_fiber()?;
        let cfg = fiber.meta.lock().unwrap().config.clone()?;
        cfg.downcast_ref::<Cfg>().cloned()
    }

    /// 等待当前飞行中的异步转换（如热替换或依赖重载）收敛；若组件处于 `Failed` 则返回其错误（对齐上游 `fiber.await()`）。
    pub async fn await_ready(&self) -> Result<(), ComponentError> {
        let Some(fiber) = self.lookup_fiber() else {
            return Ok(());
        };
        fiber.wait_idle().await;
        fiber.check_result()
    }

    /// 原地重启当前组件（递增 `revision` epoch，排空依赖者并卸载后重新运行 `apply`，对齐上游 `fiber.restart()`）。
    pub async fn restart(&self) -> Result<(), ComponentError> {
        let Some(fiber) = self.lookup_fiber() else {
            return Err(Box::new(CordisError::inactive_effect()));
        };
        fiber.check_active()?;
        fiber.wait_idle().await;
        {
            let mut meta = fiber.meta.lock().unwrap();
            if meta.disposing_permanently || meta.state == FiberState::Disposed {
                return Err(Box::new(CordisError::inactive_effect()));
            }
            meta.error = None;
            meta.revision += 1;
        }
        match fiber.state() {
            FiberState::Active | FiberState::Unloading => {
                self.ctx.unload_fiber(self.id, false).await;
            }
            FiberState::Idle | FiberState::Failed => {
                {
                    let mut meta = fiber.meta.lock().unwrap();
                    if meta.state == FiberState::Failed {
                        meta.state = FiberState::Idle;
                    }
                }
                self.ctx.try_start(&fiber);
            }
            FiberState::Disposed => {
                return Err(Box::new(CordisError::inactive_effect()));
            }
        }
        fiber.wait_idle().await;
        fiber.check_result()
    }

    /// 更新组件配置并按需驱动 `on_update` 瀑布流或重启（对齐上游 `fiber.update(config)`，`#56` / `#98` / `#134`）：
    /// - 若组件当前 `Active`，先校验新配置——校验失败直接报错并保留旧配置继续热运行（`fiber.spec.ts:396`）；
    /// - 校验通过后保存源配置，把校验/归一化后的配置送入 `on_update` 钩子链，终端默认委托 `restart()`；
    /// - 若组件当前为 `Failed` 或 `Idle`，清除失败状态并尝试以新配置恢复/等待依赖。
    pub async fn update<Cfg: Clone + Send + Sync + 'static>(
        &self,
        config: Cfg,
    ) -> Result<(), ComponentError> {
        let Some(fiber) = self.lookup_fiber() else {
            return Err(Box::new(CordisError::inactive_effect()));
        };
        fiber.check_active()?;
        fiber.wait_idle().await;

        let raw_cfg: Arc<dyn Any + Send + Sync> = Arc::new(config);
        let (was_loaded, component) = {
            let meta = fiber.meta.lock().unwrap();
            if meta.disposing_permanently || meta.state == FiberState::Disposed {
                return Err(Box::new(CordisError::inactive_effect()));
            }
            (meta.state == FiberState::Active, meta.component.clone())
        };

        let resolved_for_hooks = if was_loaded {
            if let Some(ref comp) = component {
                // 活跃组件热更新前先做配置校验：若非法则原样保留正在运行的组件与旧配置（#134）。
                comp.validate_config(Some(&raw_cfg))?
            } else {
                Some(Arc::clone(&raw_cfg))
            }
        } else {
            None
        };

        {
            let mut meta = fiber.meta.lock().unwrap();
            if meta.disposing_permanently || meta.state == FiberState::Disposed {
                return Err(Box::new(CordisError::inactive_effect()));
            }
            meta.config = Some(raw_cfg);
            meta.error = None;
        }

        if was_loaded {
            let resolved_arg =
                resolved_for_hooks.unwrap_or_else(|| Arc::new(()) as Arc<dyn Any + Send + Sync>);
            let mut hooks: Vec<UpdateHookEntry> = self
                .ctx
                .inner
                .kernel
                .global_update_hooks
                .lock()
                .unwrap()
                .clone();
            hooks.extend(fiber.update_hooks.lock().unwrap().iter().cloned());

            let hooks = Arc::new(hooks);
            let handle_for_restart = self.clone();
            let restart_result: Arc<Mutex<Option<Result<(), ComponentError>>>> =
                Arc::new(Mutex::new(None));
            let restart_result_cl = Arc::clone(&restart_result);

            fn run_update_chain(
                hooks: Arc<Vec<UpdateHookEntry>>,
                mut idx: usize,
                cfg: Arc<dyn Any + Send + Sync>,
                terminal: Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>,
            ) -> BoxFuture<'static, ()> {
                while idx < hooks.len() && hooks[idx].removed.load(Ordering::Acquire) {
                    idx += 1;
                }
                if idx < hooks.len() {
                    let cb = Arc::clone(&hooks[idx].callback);
                    let cfg_next = Arc::clone(&cfg);
                    let next: WaterfallNext<()> = Box::new(move || {
                        run_update_chain(hooks, idx + 1, cfg_next, terminal)
                    });
                    cb(cfg, next)
                } else {
                    terminal()
                }
            }

            let terminal: Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send> =
                Box::new(move || {
                    Box::pin(async move {
                        let res = handle_for_restart.restart().await;
                        *restart_result_cl.lock().unwrap() = Some(res);
                    })
                });

            run_update_chain(hooks, 0, resolved_arg, terminal).await;
            if let Some(res) = restart_result.lock().unwrap().take() {
                return res;
            }
            Ok(())
        } else {
            self.restart().await
        }
    }

    /// 原地替换该句柄背后的单个组件实现并等待收敛（单句柄便捷版 `#266`）。
    pub async fn replace(&self, new_component: Arc<dyn Component>) -> Result<(), ComponentError> {
        let Some(fiber) = self.lookup_fiber() else {
            return Err(Box::new(CordisError::inactive_effect()));
        };
        fiber.check_active()?;
        let old_runtime_key = fiber.meta.lock().unwrap().runtime_key;
        self.ctx
            .inner
            .kernel
            .comp_registry
            .lock()
            .unwrap()
            .detach_fiber(old_runtime_key, self.id);

        let raw_key = component_key(&new_component);
        let raw_erased: Arc<dyn ErasedComponent> =
            Arc::new(DynComponentAdapter(Arc::clone(&new_component)));
        let raw_pin: Arc<dyn Any + Send + Sync> = Arc::new(new_component);
        let (new_runtime_key, target_erased) = self
            .ctx
            .inner
            .kernel
            .comp_registry
            .lock()
            .unwrap()
            .mount(raw_key, raw_erased, raw_pin, self.id);

        self.ctx
            .swap_fiber_component(&fiber, Some(new_runtime_key), target_erased);
        self.await_ready().await
    }
}
