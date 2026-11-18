//! R3 一致性测试：intercept（读取时装饰 / 配置覆盖）。
//!
//! 模型（论文 `@@intercept` 槽 + Alg 6 Proxy resolve 的静态化落地）：
//! - `ctx.intercept::<T>(f)` 返回带拦截的子上下文（原上下文不变）；
//! - `get::<T>()` 在 realm 解析后，按注册序（外层先）依次应用装饰器；
//! - 配置覆盖语义：内层后应用 → 内层赢（对齐 TS `resolveConfig` 的
//!   base < 外层 intercept < 内层 intercept 合并序）；
//! - 装饰器是用户代码，锁外执行（可安全读取其他服务）。

use std::any::TypeId;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cordis::{Context, Plugin, PluginError, Service};

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

/// 计数服务：装饰器做加法，便于断言数值。
struct Counter(AtomicUsize);

impl Service for Counter {
    fn name(&self) -> &'static str {
        "counter"
    }
}

impl Counter {
    fn add(&self, n: usize) -> Arc<Counter> {
        Arc::new(Counter(AtomicUsize::new(self.0.load(Ordering::Relaxed) + n)))
    }
    fn get(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// 带字符串的服务：装饰器拼接，可断言应用顺序。
#[derive(Clone, Debug, PartialEq)]
struct Msg(String);

impl Service for Msg {
    fn name(&self) -> &'static str {
        "msg"
    }
}

// —— 基础语义 ——

#[test]
fn intercept_wraps_read_value_only_in_child() {
    let ctx = Context::new();
    ctx.provide::<Counter>(Arc::new(Counter(AtomicUsize::new(0))));

    let child = ctx.intercept::<Counter>(|c| c.add(1));

    assert_eq!(child.get::<Counter>().map(|c| c.get()), Some(1), "子上下文读到装饰后的值");
    assert_eq!(ctx.get::<Counter>().map(|c| c.get()), Some(0), "原上下文不受影响");
    assert_eq!(child.get::<Counter>().map(|c| c.get()), Some(1), "装饰不修改存储，重复读一致");
}

#[test]
fn intercept_chain_inherits_and_applies_outer_first() {
    let ctx = Context::new();
    ctx.provide::<Msg>(Arc::new(Msg("base".into())));

    let outer = ctx.intercept::<Msg>(|m| Arc::new(Msg(format!("outer+{}", m.0))));
    let inner = outer.intercept::<Msg>(|m| Arc::new(Msg(format!("inner+{}", m.0))));

    assert_eq!(
        inner.get::<Msg>().map(|m| m.0.clone()),
        Some("inner+outer+base".into()),
        "应用序：外层先、内层后（配置覆盖语义：内层赢）"
    );
    assert_eq!(outer.get::<Msg>().map(|m| m.0.clone()), Some("outer+base".into()));
    assert_eq!(ctx.get::<Msg>().map(|m| m.0.clone()), Some("base".into()));
}

#[test]
fn intercept_is_scoped_and_siblings_are_independent() {
    let ctx = Context::new();
    ctx.provide::<Counter>(Arc::new(Counter(AtomicUsize::new(0))));

    let a = ctx.intercept::<Counter>(|c| c.add(1));
    let b = ctx.intercept::<Counter>(|c| c.add(100));
    let a2 = a.intercept::<Counter>(|c| c.add(10));

    assert_eq!(a.get::<Counter>().map(|c| c.get()), Some(1));
    assert_eq!(b.get::<Counter>().map(|c| c.get()), Some(100), "兄弟互不可见");
    assert_eq!(a2.get::<Counter>().map(|c| c.get()), Some(11), "继承外层 + 自己的内层");
}

#[test]
fn intercept_is_type_scoped() {
    let ctx = Context::new();
    ctx.provide::<Counter>(Arc::new(Counter(AtomicUsize::new(0))));
    ctx.provide::<Msg>(Arc::new(Msg("base".into())));

    let child = ctx.intercept::<Counter>(|c| c.add(7));
    assert_eq!(child.get::<Counter>().map(|c| c.get()), Some(7));
    assert_eq!(
        child.get::<Msg>().map(|m| m.0.clone()),
        Some("base".into()),
        "其他类型不受拦截"
    );
}

// —— 与现有机制的交互 ——

#[test]
fn intercept_works_inside_isolated_realm() {
    let ctx = Context::new();
    let r = ctx.alloc_realm();
    let scope = ctx.scoped(r).isolate_at::<Counter>(r);
    scope.provide::<Counter>(Arc::new(Counter(AtomicUsize::new(5))));

    let child = scope.intercept::<Counter>(|c| c.add(3));
    assert_eq!(child.get::<Counter>().map(|c| c.get()), Some(8), "realm 解析 + 装饰叠加");
}

#[test]
fn interceptor_runs_outside_store_lock() {
    // 锁纪律钉死：装饰器里再读别的服务（get 内部有锁）不得死锁。
    let ctx = Context::new();
    ctx.provide::<Counter>(Arc::new(Counter(AtomicUsize::new(1))));
    ctx.provide::<Msg>(Arc::new(Msg("ok".into())));

    let inner_ctx = ctx.clone();
    let child = ctx.intercept::<Counter>(move |c| {
        let m = inner_ctx.get::<Msg>().expect("interceptor 内可读取其他服务");
        c.add(m.0.len())
    });

    assert_eq!(child.get::<Counter>().map(|c| c.get()), Some(3), "1 + len(\"ok\")");
}

// —— 与 inject 的集成：插件在 apply 里读到装饰后的配置 ——

struct Greeter {
    seen: Arc<Mutex<Vec<String>>>,
}

impl Plugin for Greeter {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<Msg>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let msg = ctx.get::<Msg>().expect("inject 就绪");
        self.seen.lock().unwrap().push(msg.0.clone());
        Ok(())
    }
}

#[test]
fn plugin_apply_reads_intercepted_config() {
    let ctx = Context::new();
    ctx.provide::<Msg>(Arc::new(Msg("hello".into())));

    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    ctx.plugin(Arc::new(Greeter {
        seen: Arc::clone(&seen),
    }));
    assert_eq!(&*seen.lock().unwrap(), &["hello".to_string()]);

    // 带覆盖的 scope：插件挂在子上下文上，apply 读到内层覆盖后的配置
    let scope = ctx.intercept::<Msg>(|m| Arc::new(Msg(format!("override:{}", m.0))));
    let seen2: Arc<Mutex<Vec<String>>> = Arc::default();
    scope.plugin(Arc::new(Greeter {
        seen: Arc::clone(&seen2),
    }));
    assert_eq!(&*seen2.lock().unwrap(), &["override:hello".to_string()]);
}

// —— FiberHandle::state()（loader 装配报告要用） ——

#[test]
fn fiber_handle_reports_state() {
    let ctx = Context::new();
    let handle = ctx.plugin(Arc::new(Greeter {
        seen: Arc::default(),
    }));
    assert_eq!(handle.state(), cordis::FiberState::Idle, "依赖未就绪 → Idle");

    ctx.provide::<Msg>(Arc::new(Msg("x".into())));
    assert_eq!(handle.state(), cordis::FiberState::Active, "依赖就绪 → Active");

    block_on(handle.dispose());
    assert_eq!(handle.state(), cordis::FiberState::Disposed);
}

// —— on_emit_scoped：监听器随 fiber 卸载自动移除 ——

use cordis::EmitEvent;

struct Ping;
impl EmitEvent for Ping {
    type Args = u32;
}

struct ListenerPlugin {
    hits: Arc<AtomicUsize>,
}

impl Plugin for ListenerPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let hits = Arc::clone(&self.hits);
        ctx.on_emit_scoped::<Ping>(move |_| {
            hits.fetch_add(1, Ordering::Relaxed);
        });
        Ok(())
    }
}

#[test]
fn scoped_listener_dies_with_fiber() {
    let ctx = Context::new();
    let hits = Arc::new(AtomicUsize::new(0));
    let handle = ctx.plugin(Arc::new(ListenerPlugin {
        hits: Arc::clone(&hits),
    }));

    ctx.emit::<Ping>(1);
    assert_eq!(hits.load(Ordering::Relaxed), 1, "fiber 存活时监听生效");

    block_on(handle.dispose());
    ctx.emit::<Ping>(2);
    assert_eq!(hits.load(Ordering::Relaxed), 1, "fiber 卸载后监听器已移除");
}
