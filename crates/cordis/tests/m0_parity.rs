//! R0 一致性测试：对照 TS M0 spike 的机制（服务挂载 / 可逆副作用 / 事件）。
//!
//! 与 TS M0 的差异：TS 用了 `serial` 事件（有返回值）与 `inject` 依赖注入，
//! 二者在设计上分属 R1；R0 覆盖「服务挂载 / 可逆副作用 / emit 事件」三项，
//! 语义与 TS 对应机制一致。inject 响应式解析与其余四事件模式在 R1 补齐（见 r1.rs）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cordis::{Context, Disposer, EmitEvent, Service};

// —— 服务示例 ——
struct Greeter;

impl Service for Greeter {
    fn name(&self) -> &'static str {
        "greeter"
    }
}

impl Greeter {
    fn greet(&self, name: &str) -> String {
        format!("hello, {name}")
    }
}

// —— 事件示例（emit 模式）——
struct Greet;

impl EmitEvent for Greet {
    type Args = String;
}

#[test]
fn service_mount_and_get() {
    let ctx = Context::new();
    ctx.provide::<Greeter>(Arc::new(Greeter));

    let got = ctx.get::<Greeter>().expect("服务应可通过 key 查回");
    assert_eq!(got.greet("maild"), "hello, maild");
}

#[test]
fn provide_is_removed_on_dispose() {
    let ctx = Context::new();
    ctx.provide::<Greeter>(Arc::new(Greeter));
    assert!(ctx.get::<Greeter>().is_some());

    // provide 把「移除」登记为可逆效果，dispose 时撤销
    futures::executor::block_on(ctx.dispose());
    assert!(ctx.get::<Greeter>().is_none());
}

#[test]
fn emit_event_calls_listener() {
    let ctx = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);

    ctx.on_emit::<Greet>(move |name| {
        assert_eq!(name, "maild");
        c.fetch_add(1, Ordering::SeqCst);
    });

    ctx.emit::<Greet>("maild".to_string());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn on_emit_disposer_is_revertible() {
    let ctx = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);

    let disposer = ctx.on_emit::<Greet>(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });

    ctx.emit::<Greet>("x".to_string());
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    match disposer {
        Disposer::Sync(f) => f(),
        _ => panic!("emit 监听器的逆应为 Sync"),
    }

    ctx.emit::<Greet>("y".to_string());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "移除后不再分发");
}

#[test]
fn reversible_sync_effect() {
    let ctx = Context::new();
    let cleaned = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&cleaned);

    ctx.effect_sync(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });

    assert_eq!(cleaned.load(Ordering::SeqCst), 0, "卸载前逆不执行");
    futures::executor::block_on(ctx.dispose());
    assert_eq!(cleaned.load(Ordering::SeqCst), 1, "卸载后逆执行");
}

#[test]
fn reversible_effect_is_lifo() {
    let ctx = Context::new();
    let order = Arc::new(Mutex::new(Vec::new()));

    let o1 = Arc::clone(&order);
    ctx.effect_sync(move || o1.lock().unwrap().push("sync1"));
    let o2 = Arc::clone(&order);
    ctx.effect_async(async move {
        o2.lock().unwrap().push("async2");
    });

    futures::executor::block_on(ctx.dispose());
    // LIFO：后注册的 async2 先执行，先注册的 sync1 后执行
    assert_eq!(*order.lock().unwrap(), vec!["async2", "sync1"]);
}
