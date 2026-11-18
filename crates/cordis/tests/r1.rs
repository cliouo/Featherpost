//! R1 一致性测试：5 种事件分发模式 + 组件（Plugin）+ inject 响应式解析 + 生命周期。

use std::any::TypeId;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use cordis::{
    BailEvent, Context, EmitEvent, ParallelEvent, Plugin, PluginError, SerialEvent, Service,
    WaterfallEvent,
};

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

// ==================== 事件：5 种模式 ====================

struct Greet;
impl SerialEvent for Greet {
    type Args = String;
    type Ret = String;
}

struct Pick;
impl BailEvent for Pick {
    type Args = u32;
    type Ret = &'static str;
}

struct Wrap;
impl WaterfallEvent for Wrap {
    type Args = String;
    type Ret = String;
}

struct Fan;
impl ParallelEvent for Fan {
    type Args = u32;
}

#[test]
fn serial_first_some_short_circuits() {
    let ctx = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));

    let c1 = Arc::clone(&calls);
    ctx.on_serial::<Greet>(move |_| {
        c1.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { None::<String> })
    });
    let c2 = Arc::clone(&calls);
    ctx.on_serial::<Greet>(move |name| {
        c2.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Some(format!("hi {name}")) })
    });
    let c3 = Arc::clone(&calls);
    ctx.on_serial::<Greet>(move |_| {
        c3.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Some("never".to_string()) })
    });

    let out = block_on(ctx.serial::<Greet>("maild".into()));
    assert_eq!(out.as_deref(), Some("hi maild"));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "第 3 个监听器被短路");
}

#[test]
fn bail_first_some_short_circuits() {
    let ctx = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));

    let c1 = Arc::clone(&calls);
    ctx.on_bail::<Pick>(move |_| {
        c1.fetch_add(1, Ordering::SeqCst);
        None
    });
    let c2 = Arc::clone(&calls);
    ctx.on_bail::<Pick>(move |_| {
        c2.fetch_add(1, Ordering::SeqCst);
        Some("handled")
    });
    let c3 = Arc::clone(&calls);
    ctx.on_bail::<Pick>(move |_| {
        c3.fetch_add(1, Ordering::SeqCst);
        Some("never")
    });

    let out = ctx.bail::<Pick>(1);
    assert_eq!(out, Some("handled"));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "第 3 个监听器被短路");
}

#[test]
fn waterfall_delegates_and_wraps() {
    let ctx = Context::new();

    ctx.on_waterfall::<Wrap>(|_args, next| {
        Box::pin(async move { format!("l1({})", next().await) })
    });
    ctx.on_waterfall::<Wrap>(|_args, next| {
        Box::pin(async move { format!("l2({})", next().await) })
    });

    let fut = ctx.waterfall::<Wrap, _>(
        "hello".to_string(),
        |args| Box::pin(async move { format!("inner:{args}") }),
    );
    let out = block_on(fut);
    assert_eq!(out, "l1(l2(inner:hello))");
}

#[test]
fn waterfall_short_circuit() {
    let ctx = Context::new();

    ctx.on_waterfall::<Wrap>(|args, _next| {
        // 不调用 next()，直接短路
        Box::pin(async move { format!("stopped:{args}") })
    });
    ctx.on_waterfall::<Wrap>(|_args, next| {
        Box::pin(async move { format!("unreached({})", next().await) })
    });

    let fut = ctx.waterfall::<Wrap, _>(
        "x".to_string(),
        |_args| Box::pin(async move { "inner".to_string() }),
    );
    let out = block_on(fut);
    assert_eq!(out, "stopped:x");
}

#[test]
fn parallel_runs_all_listeners() {
    let ctx = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));

    for _ in 0..4 {
        let c = Arc::clone(&calls);
        ctx.on_parallel::<Fan>(move |_n| {
            c.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {})
        });
    }

    block_on(ctx.parallel::<Fan>(0));
    assert_eq!(calls.load(Ordering::SeqCst), 4, "全部监听器并发执行");
}

// ==================== 组件 + inject 响应式解析 ====================

struct Db;

impl Service for Db {
    fn name(&self) -> &'static str {
        "db"
    }
}

/// 提供 Db 的组件。
struct ProviderPlugin;

impl Plugin for ProviderPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![]
    }
    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        ctx.provide::<Db>(Arc::new(Db));
        Ok(())
    }
}

/// 依赖 Db 的组件：启动置位 started，卸载置位 cleaned。
struct DependentPlugin {
    started: Arc<AtomicBool>,
    cleaned: Arc<AtomicBool>,
}

impl Plugin for DependentPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<Db>()]
    }
    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        self.started.store(true, Ordering::SeqCst);
        let cleaned = Arc::clone(&self.cleaned);
        ctx.effect_sync(move || {
            cleaned.store(true, Ordering::SeqCst);
        });
        Ok(())
    }
}

#[test]
fn inject_reactive_start() {
    let ctx = Context::new();
    let started = Arc::new(AtomicBool::new(false));
    let cleaned = Arc::new(AtomicBool::new(false));

    let _handle = ctx.plugin(Arc::new(DependentPlugin {
        started: Arc::clone(&started),
        cleaned: Arc::clone(&cleaned),
    }));

    // 依赖缺失：apply 未运行
    assert!(!started.load(Ordering::SeqCst));

    // 提供依赖 → 触发启动
    ctx.provide::<Db>(Arc::new(Db));
    assert!(started.load(Ordering::SeqCst), "依赖就绪后 apply 运行");
}

#[test]
fn inject_cascade_teardown() {
    let ctx = Context::new();

    let provider = ctx.plugin(Arc::new(ProviderPlugin));
    let started = Arc::new(AtomicBool::new(false));
    let cleaned = Arc::new(AtomicBool::new(false));
    let _dep = ctx.plugin(Arc::new(DependentPlugin {
        started: Arc::clone(&started),
        cleaned: Arc::clone(&cleaned),
    }));

    assert!(started.load(Ordering::SeqCst), "provider 就绪后 dependent 启动");
    assert!(!cleaned.load(Ordering::SeqCst));

    // 卸载 provider → Db 撤销 → dependent 级联卸载
    block_on(provider.dispose());
    assert!(cleaned.load(Ordering::SeqCst), "依赖撤销后 dependent 被级联卸载");
}

#[test]
fn plugin_effect_lifo_dispose() {
    let ctx = Context::new();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));

    struct P(Arc<std::sync::Mutex<Vec<&'static str>>>);
    impl Plugin for P {
        fn inject(&self) -> Vec<TypeId> {
            vec![]
        }
        fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
            let o1 = Arc::clone(&self.0);
            ctx.effect_sync(move || o1.lock().unwrap().push("first"));
            let o2 = Arc::clone(&self.0);
            ctx.effect_async(async move {
                o2.lock().unwrap().push("second");
            });
            Ok(())
        }
    }

    let handle = ctx.plugin(Arc::new(P(Arc::clone(&order))));
    block_on(handle.dispose());
    assert_eq!(*order.lock().unwrap(), vec!["second", "first"]);
}

// ==================== 锁纪律：panic 不污染事件总线 ====================

struct Ping;
impl EmitEvent for Ping {
    type Args = String;
}

#[test]
fn panicking_listener_does_not_poison_bus() {
    let ctx = Context::new();

    // 正常监听器先注册
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    ctx.on_emit::<Ping>(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });

    // panic 监听器后注册
    ctx.on_emit::<Ping>(|_| panic!("boom"));

    // 第一次 emit：正常监听器先跑（计数 +1），随后 panic 监听器 panic，被捕获
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.emit::<Ping>("x".to_string());
    }));
    assert!(r.is_err(), "panic 应传播到调用者");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "panic 前正常监听器已执行");

    // 关键：总线未中毒——panic 后仍能注册新监听器（若锁被污染，此处 .lock().unwrap() 会 panic）
    ctx.on_emit::<Ping>(|_| {});

    // 第二次 emit：正常监听器再次执行，证明分发链路未受影响
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.emit::<Ping>("y".to_string());
    }));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "panic 后分发仍正常，锁未中毒");
}
