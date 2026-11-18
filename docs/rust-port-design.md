# cordis → Rust 内核移植设计

> 状态：已落地并同步至上游 `cordis v4.0.0-rc.13`（`bc9fae7a92de7f74417e54eaf3decdecf71f937a`）。
> 规格来源：上游论文 [cordiverse/paper](https://github.com/cordiverse/paper)（形式化定义）· 上游 [cordiverse/cordis](https://github.com/cordiverse/cordis) `packages/core/src`（TS 可执行规格，`v4.0.0-rc.13`）· [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)（最佳实践）。仓库内的 `paper.pdf` 与 `vendor/cordis` 副本已随 [ADR-0047](decisions/0047-decoupled-brand-and-open-source-readiness.md) 移除。
> 目标：把「时空可组合性」方法论移植为 Rust 库，供 maild（对外品牌 Featherpost）使用。**移植方法论，不逐行移植 TS API。**

## 1. 范围与非目标

**范围**（对应论文三层中的前两层）：

- core 库：effect tracking（可逆效果）+ coeffect resolution（响应式共效果）+ 组件生命周期 + 事件系统 + 作用域/拦截。
- 组件 loader：声明式配置 + 装配（HMR 二期单独列）。

**非目标 / 明确放弃的 TS 特性**：

| TS 特性 | Rust 替代 | 说明 |
|---|---|---|
| Proxy 动态属性访问（`ctx.foo`） | 类型键控 getter（`ctx.get::<Foo>()`） | 论文 §6.4 明说这层可「编译期调解」；Rust 泛型 + TypeId 原生做到，且是**净增强** |
| 声明合并（`declare module` 注册事件/服务） | trait + TypeId 显式注册 | Rust 无全局声明合并 |
| 事件名字符串键 | TypeId 事件键 | 牺牲名字可读性，换类型安全 |
| JS 动态热替换 | 静态编译 + 可选 cdylib | HMR 在 Rust 是动态库重载，工程量不同 |

## 2. 论文形式化 → Rust 构造映射

### 2.1 可逆效果（时间可组合性）

| 论文 | Rust |
|---|---|
| 效果 `Γ → Γ × (Γ → Γ)`（应用变换 + 返回逆） | `fn(&mut Ctx) -> Disposer` |
| 效果上下文 `𝜕Γ = (𝛾, 𝜑)` | `Context { store: 𝛾, disposers: 𝛗（LIFO 栈） }` |
| `trackΓ(f,g)`：应用 f、把逆 g 压栈 | `ctx.effect(f)`：执行 f，返回的 disposer 压栈顶 |
| 逆在效果应用点提供（`𝔈*Γ` per-state inverse） | 注册函数返回 guard，guard Drop 即逆 |
| LIFO 回滚无需假设（Theorem 16） | disposer 栈 LIFO 弹出 |
| 独立效果任意序回滚（Corollary 21） | 组件 teardown 顺序的语义保证 |
| 加载组件 = 应用效果序列 + 累积 inverse；卸载 = 应用 𝜑 | fiber 生命周期：mount 累积 disposer，dispose 逆序执行 |

论文 §1.1 直接点明：**静态场景下时间可组合性退化为 RAII**。Rust 的 Drop 就是 RAII，所以「逆」这一半 Rust 天然更顺；要补的是「动态场景」（运行时加载/卸载组件）的显式 disposer 栈。

### 2.2 响应式共效果（空间可组合性）

| 论文 | Rust |
|---|---|
| 共效果上下文 `Σ = (k:K) ⇀ 𝒱k`（依赖类型族） | 类型键控表 `HashMap<TypeId, Arc<dyn Any + Send + Sync>>` |
| `get(k)` | `ctx.get::<T>() -> Option<Arc<T>>` |
| `set(k,v) : 𝔈*Σ`（**共效果操作本身就是效果**） | `ctx.provide::<T>(svc) -> ServiceGuard`（Drop 移除） |
| `notify(ctx, keys)`：key 变化唤醒声明它的组件 | waiter 图：每个 TypeId 记录等待者，provide/revoke 时 refresh |
| 绑定仅当安装者 fiber ACTIVE 时可见 | 服务 `ServiceState` + 「providing」状态 |
| isolate / intercept（realm 表 `𝜌`） | 作用域 id / 元数据拦截（R2，见 §6） |

`set(k,v) : 𝔈*Σ` 是整篇论文最关键的句子之一：**注册服务 = 一个可逆效果**。Rust 里 `provide` 返回 `ServiceGuard`，Drop 时从表移除——这就是它的直接实现，且是编译期 + RAII 双保险。

### 2.3 组件与可配置组件

论文：组件 = `inject`（共效果规范 𝑑）+ `apply`（效果函数）。同步上游 `v4.0.0-rc.13` 后，Rust 同时提供现代 `Component` / `ConfigurableComponent` 与 100% 向后兼容的 `Plugin`：

```rust
pub trait Component: Send + Sync + 'static {
    fn name(&self) -> Option<&'static str> { None }
    fn inject(&self) -> Vec<TypeId> { Vec::new() }
    fn inject_intercepts(&self) -> HashMap<TypeId, InterceptFn> { HashMap::new() }
    fn apply(&self, ctx: &Context) -> Result<(), ComponentError>;
}

pub trait ConfigurableComponent: Send + Sync + 'static {
    type Config: Clone + Send + Sync + 'static;
    fn name(&self) -> Option<&'static str> { None }
    fn inject(&self) -> Vec<TypeId> { Vec::new() }
    fn inject_intercepts(&self) -> HashMap<TypeId, InterceptFn> { HashMap::new() }
    /// 对齐上游 Standard Schema v1 校验/归一化（#189 / #134）
    fn validate(&self, config: Self::Config) -> Result<Self::Config, ComponentError> { Ok(config) }
    fn apply(&self, ctx: &Context, config: Self::Config) -> Result<(), ComponentError>;
}
```

既有 `Plugin` 自动实现 `Component`（blanket impl），并通过 `ComponentRegistry` 支持按组件实例去重（`ctx.use_component`）与无去重独立挂载（`ctx.component` / `ctx.use_with`）。

### 2.4 上下文访问（`get` 与 `locate` 双视图）

论文 Algorithm 6（Proxy resolve）沿 fiber 链向上走：在「未提交的 inject」上访问抛 `INACTIVE_ACCESS`，到根仍未声明抛 `UNDECLARED_ACCESS`。论文 §6.4 指出这「原则上可编译期检测，靠宿主语言的类型级依赖声明 + 编译期元编程」。

Rust 用泛型参数把「依赖 `T`」变成编译期强类型，同时区分两种读取语义：

- **`ctx.get::<T>() -> Option<Arc<T>>`（实时存储视图）**：严格查询当前 `realm_of::<T>()` 的活跃绑定；提供者一旦进入卸载或撤回即返回 `None`。
- **`ctx.locate::<T>() -> Option<Arc<T>>`（已提交继承视图，对齐上游 `reflect.ts`）**：沿组件祖先链解析本 fiber 的 `provisions` 与声明依赖祖先的 `committed` 快照——既严格遵守显式 `isolate` / `isolate_at` 隔离边界（`#209`），又在消费者异步清理期间持续保持上游服务可读（直至消费者逆执行完毕才释放提供者，`#196`）。

## 3. 核心类型骨架

（草图，非最终 API）

```rust
// —— 效果：逆在应用点提供 ——
pub struct Disposer { f: Box<dyn FnOnce()> }
impl Disposer { pub fn new<F: FnOnce() + 'static>(f: F) -> Self { Self { f: Box::new(f) } } }
impl Drop for Disposer { fn drop(&mut self) { (self.f)(); } }  // 简化示意；实际用 scopeguard

// —— 类型键控服务表（共效果上下文 Σ） ——
pub trait Service: Any + Send + Sync { fn name(&self) -> &'static str; }

#[derive(Default)]
pub struct Registry { map: HashMap<TypeId, (Arc<dyn Any + Send + Sync>, ServiceState)> }

impl Registry {
    pub fn provide<T: Service>(&mut self, svc: Arc<T>) { /* set(k,v) */ }
    pub fn get<T: Service>(&self) -> Option<Arc<T>> {
        self.map.get(&TypeId::of::<T>())
            .and_then(|(a, _)| a.clone().downcast::<T>().ok())
    }
}

// —— 上下文（效果上下文 𝜕Γ = 𝛾 × 𝜑） ——
pub struct Context {
    store: Rc<RefCell<Registry>>,                        // 𝛾
    disposers: RefCell<Vec<Disposer>>,                   // 𝜑（LIFO）
    fibers: Rc<RefCell<Vec<Fiber>>>,                     // 活跃组件，供 notify 遍历
    waiters: Rc<RefCell<HashMap<TypeId, Vec<FiberId>>>>, // 响应式共效果
}

impl Context {
    /// trackΓ(f, g)：应用 f，把逆 g 压栈
    pub fn effect(&self, f: impl FnOnce() -> Disposer) { let d = f(); self.disposers.borrow_mut().push(d); }
    pub fn get<T: Service>(&self) -> Option<Arc<T>> { self.store.borrow().get::<T>() }
    pub fn provide<T: Service>(&self, svc: Arc<T>) -> ServiceGuard { /* set(k,v) : 𝔈*Σ + notify */ }
    pub fn on<E: Event>(&self, listener: ...) -> Disposer { /* 事件注册 = 可逆效果 */ }
}
```

`ServiceGuard` 是关键：`provide` 时写表 + notify 等待者，返回的 guard Drop 时移除 + 再 notify。这就是 `set(k,v) : 𝔈*Σ` 的 Rust 落地。

## 4. 三个难点逐一设计

### 4.1 类型键控服务表（替代 Proxy）

- `TypeId` 作键，`Arc<dyn Any + Send + Sync>` 存值，`downcast` 取回强类型。
- 一个服务一个类型（`Greeter` 实现 `Service`），`ctx.get::<Greeter>()`。
- 代价：同名多实例（per-account）需引入 realm/scope（§6），否则一个类型只能存一个实例。
- 与 cordis 两层解析（key → realm → value）对齐：v1 单层（realm 恒等），v2 加 realm。

### 4.2 响应式共效果（notify / refresh）

论文 Algorithm 3：`notify(ctx, keys)` 遍历所有 fiber，若 key ∈ fiber.inject 且 isolate 匹配则 `refresh(fiber)`。

Rust 设计：

- `waiters: HashMap<TypeId, Vec<FiberId>>` 记录「谁在等哪个服务」。
- `provide::<T>` / guard Drop 时，取出 `waiters[TypeId::of::<T>()]`，逐个 `refresh`。
- `refresh(fiber)`：重解析 fiber.inject 每个 TypeId；全就绪 → 启动（调 apply + 累积 effect）；有缺 → 标记等待。
- 论文强调的「withdrawal 提前可见」：provider 进入 UNLOADING 时先「停止提供」再执行逆，让依赖者先于绑定移除而开始 teardown。Rust 对应：guard Drop 前先置 `ServiceState::Unloading` + notify，再移除。

这是移植最需小心的一块，但有明确算法参照（论文 Alg 2/3），不是玄学。

### 4.3 事件系统（5 种分发模式）

从 TS `events.ts` 精确转写的语义表：

| 模式 | await？ | 顺序 | 返回值 | Rust 实现要点 |
|---|---|---|---|---|
| `emit` | 否 | 注册序观察 | 无 | 同步迭代调用，忽略返回值 |
| `parallel` | 是 | 并发扇出 | 无 | `futures::future::join_all` |
| `serial` | 是 | 注册序 | 首个 bailed 值，短路 | 顺序 await，遇 `Some` 即返回 |
| `bail` | 否 | 注册序 | 首个 bailed 值，短路 | 迭代，遇 `Some` 即返回 |

> 语义修正（对照 `events.ts` 源码）：serial/bail 都是「**首个 bailed 值短路返回**」，不是「末值」。Rust 用 `Option<Ret>` 表达 cordis 的 `isBailed` 判定（`null`/`false`/`undefined` → `None` 不拦截；其余 → `Some(v)` 拦截）。
| `waterfall` | 是 | 注册序中间件 | 链式返回值 | 监听器收 `(args, next)`，`next()` 调下游，返回值逐层包裹 |

waterfall 是中间件环绕（对应 cordis-primer 的 `ctx.waterfall` 语义，tower 的 Service/Layer 精神，但按序而非洋葱反转）。

**已定稿：trait-per-mode + 模式命名方法**（对应 TS 的声明合并 + 分发方法名即模式）：

```rust
pub trait EmitEvent: 'static       { type Args: Clone + Send + 'static; }
pub trait SerialEvent: 'static     { type Args: Send + 'static; type Ret: Send + 'static; }
pub trait ParallelEvent: 'static   { type Args: Send + 'static; }
pub trait BailEvent: 'static       { type Args: Send + 'static; type Ret: Send + 'static; }
pub trait WaterfallEvent: 'static  { type Args: Send + 'static; type Ret: Send + 'static; }

pub struct Greet;
impl SerialEvent for Greet { type Args = String; type Ret = String; }

ctx.on_serial::<Greet>(|name| async { format!("hi {name}") });
let s = ctx.serial::<Greet>("maild".into()).await;
```

方法名即模式（`on_emit` / `on_waterfall` / `on_serial` / `on_parallel` / `on_bail`，与 cordis 分发方法一一对应）。事件作者 2 行定义一个事件。同步上游 `rc.13` 后，全部 5 种模式均额外提供：

- `EventOptions { prepend, global }`（`on_*_with`，支持队首插入与跨 scope 全局监听）；
- 一次性监听 `on_*_once` / `on_*_once_with` / `on_emit_once_scoped`（`#296`）；
- **分发期即时移除（`#242`）**：监听器持有原子 `removed` 标记，在 `emit` / `bail` / `serial` / `parallel` / `waterfall` 任一模式分发中途调用 disposer 移除后续监听器立即生效，而分发中新注册的监听器推迟到下轮生效；
- **空事件桶自动回收（`#51`）**：最后一个监听器注销时立即清理对应 `TypeId` 桶，避免动态事件造成哈希表膨胀。

### 4.4 异步清理（async drop 问题）

TS 里 `dispose` 可 `async`；Rust 的 `Drop` 不能 async。cordis 的解法是**所有清理统一走 `ctx.effect` 返回的「逆」**（普通函数 / Promise / 同步生成器 / 异步生成器四形态），`fiber.dispose` 异步地 LIFO await。**已定稿：统一 `Disposer` 枚举，忠实对应此模型**：

```rust
pub enum Disposer {
    Sync(Box<dyn FnOnce()>),
    Async(Pin<Box<dyn Future<Output = ()> + Send>>),
}

impl Context {
    pub fn effect_sync<F: FnOnce() + 'static>(&self, f: F) { /* push Disposer::Sync */ }
    pub fn effect_async<F: Future<Output = ()> + Send + 'static>(&self, f: F) { /* push Disposer::Async */ }
}

impl Fiber {
    pub async fn dispose(&mut self) {
        while let Some(d) = self.disposers.pop() {   // LIFO，对应 cordis 逆序
            match d { Disposer::Sync(f) => f(), Disposer::Async(f) => f.await }
        }
    }
}
```

规则与不变量：

- 同步清理走 RAII / `effect_sync`；异步清理走 `effect_async`；**禁止在 `Drop` 里做阻塞 I/O**。
- 废弃此前 `AsyncService` trait 方案——不需要额外 trait，组件作者按资源决定用哪个 `effect_*` 注册逆，完全对齐 cordis 的「统一逆」模型。
- 异步清理不享受 panic 自动 Drop（daemon panic 即进程退出，可接受）；异步 teardown 只由显式 `dispose().await` 驱动。

## 5. 组件生命周期状态机

对应 TS `fiber.ts` 的 `FiberState` 与论文 §4.3.3（同步上游 `v4.0.0-rc.13`）：

```
IDLE (PENDING) ──(inject 全就绪 + validate/apply 成功)──▶ ACTIVE ──(依赖撤回/重载)──▶ UNLOADING ──(逆执行完)──▶ IDLE
      │                                                      │                                                │
      ├──(validate/apply 报错 + 完整回滚)──▶ FAILED          └──(显式 dispose)──────▶ UNLOADING ──(逆执行完)──▶ DISPOSED
      │       (不随 refresh 自动重入；仅 update/restart/replace 清除错误后恢复)
      └──────────────────────────────────────────────────────────────────────────────────(新依赖就绪即重装)────┘
```

- `fiber` 并发控制：`in_flight` + `idle_waiters` + `deferred_task` 串行化在途异步转换，`FiberHandle::await_ready()` 精确收敛并暴露 `Failed` 错误。
- **失败态隔离与完整回滚（`#98` / `#133`）**：`validate` 或 `apply` 失败时，不仅撤回中途 `provide` 的服务，还按 LIFO 完整回滚已注册的同步/异步 `effect_*` 与 `on_*_scoped` 监听器，随后落入 `FiberState::Failed`；此状态下上游服务变动不会误触发重入，须显式调用 `update` / `restart` / `replace` 恢复。
- **响应式卸载 vs 永久销毁**：因依赖撤回、覆盖或热替换触发的响应式卸载在逆执行完后回到 `FiberState::Idle`（新绑定就绪即自动重装回 `Active`）；显式调用 `dispose()` 则标记失活（后续在旧上下文注册副作用报 `CordisErrorCode::InactiveEffect`）并最终收敛至 `FiberState::Disposed`（`#133`）。
- **提供者等待已提交消费者异步排空（`#196`）**：每个服务实现拥有单调递增 `uid` 与 `dependents` 计数。无论按「先父后子 / 先提供者后消费者 / 先消费者后提供者 / 单独 `ProvideHandle::withdraw()`」何种顺序卸载，提供者在撤下实时绑定后都会等待 `wait_no_dependents().await`，保证消费者在整个异步清理周期内都能通过 `ctx.locate::<T>()` 安全读取未关闭的上游资源；`UnloadGuard` 在消费者逆 panic 时亦能兜底释放引用计数。
- **热更新、重启与原地替换（`#56` / `#134` / `#189` / `#266`）**：
  - `ctx.on_update::<Cfg>(...)` 挂接 fiber 局部更新瀑布流（重载时自动清理，不跨轮次累积）；`handle.update(cfg)` 在活跃态先执行 `validate`（失败则无损保留正在运行的旧配置与实例），再驱动更新链或委托 `handle.restart()`；
  - `ctx.replace_component(&old, new)` / `ctx.replace_configurable(&old, new)` 原地迁移全部已挂载 fiber 句柄、建立组件转发别名，并按新组件 `inject` 增量收放依赖。

## 6. 作用域与拦截（isolate / intercept）

论文两层解析 `key → realm → value`。R0/R1 为单层（realm 恒等）。R2–R4（`rc.13`）已完整实现：

- **服务隔离（per-key，忠实论文 + `#209` 隔离代数）**：`ctx.isolate::<T>()` / `ctx.isolate_at::<T>(realm)` 派生子 Context，服务表按 `(realm, TypeId)` 复合键；`get::<T>()` **严格**解析 `realm_of::<T>()`（未隔离的 key 归 ROOT = 共享全局绑定；隔离的 key = 独立绑定，须在 scope 内自行 provide）。每次调用 `isolate` / `isolate_at` 同时递增该 key 的 `isolation_epochs`，使 `ctx.locate::<T>()` 跨越显式隔离边界时立即停止向上回退，绝不穿透读取外层祖先的 `committed` 服务或注入拦截（`#209`）。
- **事件作用域**：`ctx.scoped(realm)` 派生事件 scope，分发只达「本 scope + ROOT + `EventOptions { global: true }`」的监听器（全局插件见所有账户，账户插件只见本账户）。
- **设计陷阱（已踩并修正）**：曾尝试「整上下文 realm + 回退 root」——scope 绑定被移除时，依赖者会回退到 root 而保持 Active 且持有过期引用，级联卸载失效。**严格解析（无回退）是级联正确性的前提**，与论文「独立绑定」语义一致。
- **intercept（上下文拦截 + 注入点拦截 `#206`）**：
  - 上下文级：`ctx.intercept::<T>(f: Fn(Arc<T>) -> Arc<T>)` 返回带拦截的子上下文（原上下文不变，子上下文继承，兄弟互不可见）；
  - 注入点级：`Component::inject_intercepts` / `ctx.inject_with_intercepts` 支持为声明的依赖绑定组件级拦截器；
  - 合成顺序对齐上游 `#206`：`base -> 声明祖先的 inject intercept -> 外层至内层 context intercepts`；且若服务由当前组件自身（或其祖先 `provisions`）直接提供，则按上游规则绕过注入点拦截、仅应用上下文拦截。装饰器一律锁外执行（可安全读取其他服务）。配置侧封装：`maild::config::Merge` + `intercept_config`。

maild 多账户模式：`let account = ctx.scoped(r).isolate_at::<Receive>(r).isolate_at::<Store>(r);`（事件 scope + 每账户服务隔离，共享服务不 isolate 即可见 root）。

## 7. 里程碑（R0–R4）

| 里程碑 | 内容 | 验收 |
|---|---|---|
| R0 ✅ | Context + 服务表 + provide/get + effect/Disposer + emit 单事件 | 6 测试全过（`cargo test`），见 `crates/cordis/tests/m0_parity.rs` |
| R1 ✅ | 全部 5 事件模式 + inject 响应式解析（waiters/refresh）+ 生命周期状态机 | 14 测试全过（`cargo test`），见 `crates/cordis/tests/r1.rs`；级联卸载的时序精化（论文「withdrawal 提前可见」）R1.5 ✅ 已完成，见 `tests/r1_5.rs` |
| R2 ✅ | realm 作用域（per-key isolate + 事件 scope） | 22 测试全过（`cargo test`），见 `crates/cordis/tests/r2.rs`；intercept 留待 R3 |
| R3 ✅ | intercept（读取装饰）+ loader：声明式配置（TOML）+ 装配 | 内核 30 测试全过（r3.rs 8 测试钉死 intercept 语义）；`crates/maild`（loader 8 测试）+ `examples/assemble.rs` 演示守护进程从配置装配插件 |
| R4 ✅ | 领域服务实战：receive(imap-idle)/store(rusqlite)/notify/otp | `crates/maild`：SMTP 发信→IMAP IDLE 收信→过滤链→store 落库→otp 提取→通知；e2e（hermetic 假服务器）全通；守护进程 `maild` 二进制 + `maild.example.toml`。⚠️ Mailpit 无 IMAP（已实测），假服务器见 `tests/common/fake_mail.rs` |
| R4.5 第一批 ✅ | 多账户装配（§6 模式实战化）+ otp 去重持久化 + 轮询兜底 | `[[accounts]]` + `scoped(r).isolate_at::<Receive>(r).isolate_at::<ReceiveConfig>(r)`；`Accounts` 服务（spine ctx.accounts 最小形态）；e2e 钉死事件隔离（账户 scope 探针只见本账户）；store `otp_notifications` 表使重放只通知一次。真实服务器验证/STARTTLS 受环境限制留待后续 |
| 上游 `v4.0.0-rc.13` 同步 ✅ | 同步 `cordiverse/cordis` `rc.8 → rc.13`（`bc9fae7`）全部核心语义 | （当时的）`vendor/cordis` 与 `crates/cordis` 同步对齐：`FiberState::Failed` 非重入与完整回滚（`#98`/`#133`）、异步消费者排空保活提供者（`#196`）、注入拦截（`#206`）、`isolate` 边界阻断继承（`#209`）、分发期即时移除监听（`#242`）、空事件桶回收（`#51`）、`once`（`#296`）、配置校验/热更新（`#56`/`#134`/`#189`）、原地 `replace`（`#266`）；内核 52 测试全过，见 `crates/cordis/tests/r4_rc13_sync.rs` |

HMR（动态库 + 重载）单独列，二期。

## 8. 一致性测试（parity with TS）

TS M0 spike 已把 4 机制 + 5 模式语义验证为「可执行规格」。Rust 版写等价断言，两边跑同一组行为用例（服务挂载 / 注入就绪启动 / 各事件模式返回值 / dispose 回滚）。TS 不再作为运行时，只作为**规格与对照**保留在 `packages/spike/`。

## 9. 决策与开放问题

- **为什么不直接用 shaku / tower / bevy_ecs**：各有其一（DI / 中间件 / 组合），但都不具备「可逆效果 + 响应式共效果」的合一语义；core 仅 1848 行 TS，自研有界且完全贴合方法论。
- **事件类型化 API**：已定 trait-per-mode + 模式命名方法（§4.3）；proc-macro 留作后续糖。
- **第三方插件生态**：v1 全部静态编译进二进制（零 ABI 风险）；动态插件（cdylib + 稳定 ABI）是远期。
- **异步 teardown 协议**：已定统一 `Disposer` 枚举（Sync|Async）+ `dispose()` LIFO await（§4.4），废弃 `AsyncService` trait 方案。
- **bail/serial 返回值**：已定 `Option<Ret>`（`None`=不拦截，`Some(v)`=拦截）。语义边界：`Some(false)` 表示「以 false 拦截」，与 cordis 的 `false`=不拦截不同——从 cordis 移植时 `return false` 必须译成 `None`，由语义测试钉死。
- **锁纪律与无环引用纪律**：全模块所有 `std::sync::Mutex` 均为严格叶锁（rank-0）——锁内只做 O(1)/内存态读写，绝不嵌套加锁、绝不跨用户回调/通道发送/`.await` 持锁；同时内部 `Fiber` 逆栈与事件/更新钩子统一持有 `Weak<SharedKernel>` / `Weak<Fiber>`，确保直接 `drop(ctx)`（即使未显式 `dispose().await`）也能立即析构全部服务与 RAII 文件锁，零引用环泄漏。
- **运行时**：tokio（current_thread 单线程足够，邮件是 I/O 密集非 CPU 密集）；更极致可评估单线程 + 手写 poll，但收益远小于复杂度，v1 用 tokio。

## 附：论文要点速查

- 作者：Yifan Shi, Wei Zhang（北大），Tianyi Cui（DeepSeek-AI）。88 页。
- §1.1 两维度定义；§1.2.1 用 VSCode 扩展宿主说明「时间可组合性缺失」（无法卸载单扩展）。
- §3.1 可逆效果：`𝜕Γ=(𝛾,𝜑)`、`trackΓ`、`𝔈*Γ`、`⋄` 组合、独立性（Theorem 16 LIFO / Corollary 21 任意序）。
- §3.2 响应式共效果：`Σ=(k:K)⇀𝒱k`、`get/set`、`set(k,v):𝔈*Σ`（共效果即效果）。
- §5.1 实现：`@@store` / `@@isolate` / `@@intercept` 三槽、两层解析、Alg 1 effect tracking / Alg 2 coeffect ops / Alg 3 notify / Alg 6 Proxy resolve。
- §6.4 明示 Proxy 的运行时检查可编译期调解——Rust 泛型即此调解。
