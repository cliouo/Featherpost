//! R2 一致性测试：realm 作用域。
//!
//! 模型（论文 `key → realm → value` 两层解析的忠实简化）：
//! - 服务：per-key `isolate` —— 未隔离的 key 共享 ROOT 绑定（严格解析，无回退）；
//!   隔离后 `get::<T>()` 解析到独立绑定，须在 scope 内自行 provide。
//! - 事件：`scoped(realm)` —— 分发只达「本 scope + ROOT」的监听器。

use std::any::TypeId;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cordis::{Context, EmitEvent, Plugin, PluginError, Service};

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

/// 可区分实例的服务。
struct Db(u32);

impl Service for Db {
    fn name(&self) -> &'static str {
        "db"
    }
}

struct Ping;
impl EmitEvent for Ping {
    type Args = String;
}

// —— 服务：隔离 / 共享 ——

#[test]
fn non_isolated_sees_root_service() {
    let ctx = Context::new();
    ctx.provide::<Db>(Arc::new(Db(0)));
    let scope = ctx.scoped(ctx.alloc_realm());
    assert_eq!(scope.get::<Db>().map(|d| d.0), Some(0), "未隔离的 key 共享 root 绑定");
}

#[test]
fn isolated_is_independent() {
    let ctx = Context::new();
    ctx.provide::<Db>(Arc::new(Db(0)));

    let r = ctx.alloc_realm();
    let scope = ctx.scoped(r).isolate_at::<Db>(r);

    // 隔离后：scope 内无绑定（不回退），root 绑定不受影响
    assert!(scope.get::<Db>().is_none(), "隔离后独立绑定，初始为空");
    assert_eq!(ctx.get::<Db>().map(|d| d.0), Some(0));

    // scope 内 provide 自己的实例
    scope.provide::<Db>(Arc::new(Db(1)));
    assert_eq!(scope.get::<Db>().map(|d| d.0), Some(1));
    assert_eq!(ctx.get::<Db>().map(|d| d.0), Some(0), "root 不受 scope 覆盖");
}

#[test]
fn two_isolated_scopes_are_independent() {
    let ctx = Context::new();
    let ra = ctx.alloc_realm();
    let rb = ctx.alloc_realm();
    let a = ctx.isolate_at::<Db>(ra);
    let b = ctx.isolate_at::<Db>(rb);

    a.provide::<Db>(Arc::new(Db(1)));
    b.provide::<Db>(Arc::new(Db(2)));

    assert_eq!(a.get::<Db>().map(|d| d.0), Some(1));
    assert_eq!(b.get::<Db>().map(|d| d.0), Some(2));
    assert!(ctx.get::<Db>().is_none(), "root 没有绑定");
}

// —— 事件：scope 分发 ——

#[test]
fn events_are_scope_filtered() {
    let ctx = Context::new();
    let a = ctx.scoped(ctx.alloc_realm());
    let b = ctx.scoped(ctx.alloc_realm());

    let global = Arc::new(AtomicUsize::new(0));
    let g = Arc::clone(&global);
    ctx.on_emit::<Ping>(move |_| {
        g.fetch_add(1, Ordering::SeqCst);
    });

    let ac = Arc::new(AtomicUsize::new(0));
    let a2 = Arc::clone(&ac);
    a.on_emit::<Ping>(move |_| {
        a2.fetch_add(1, Ordering::SeqCst);
    });

    let bc = Arc::new(AtomicUsize::new(0));
    let b2 = Arc::clone(&bc);
    b.on_emit::<Ping>(move |_| {
        b2.fetch_add(1, Ordering::SeqCst);
    });

    // root 分发：只有全局
    ctx.emit::<Ping>("x".into());
    assert_eq!(global.load(Ordering::SeqCst), 1);
    assert_eq!(ac.load(Ordering::SeqCst), 0);
    assert_eq!(bc.load(Ordering::SeqCst), 0);

    // scope a 分发：全局 + a
    a.emit::<Ping>("y".into());
    assert_eq!(global.load(Ordering::SeqCst), 2);
    assert_eq!(ac.load(Ordering::SeqCst), 1);
    assert_eq!(bc.load(Ordering::SeqCst), 0);

    // scope b 分发：全局 + b
    b.emit::<Ping>("z".into());
    assert_eq!(global.load(Ordering::SeqCst), 3);
    assert_eq!(ac.load(Ordering::SeqCst), 1);
    assert_eq!(bc.load(Ordering::SeqCst), 1);
}

// —— inject：realm 解析 ——

/// 记录 apply 时解析到的 Db 实例 id 的插件。
struct DbReader(Arc<Mutex<Option<u32>>>);

impl Plugin for DbReader {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<Db>()]
    }
    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        *self.0.lock().unwrap() = ctx.get::<Db>().map(|d| d.0);
        Ok(())
    }
}

#[test]
fn inject_resolves_root_when_not_isolated() {
    let ctx = Context::new();
    ctx.provide::<Db>(Arc::new(Db(0)));
    let scope = ctx.scoped(ctx.alloc_realm());

    let seen = Arc::new(Mutex::new(None));
    scope.plugin(Arc::new(DbReader(Arc::clone(&seen))));
    assert_eq!(*seen.lock().unwrap(), Some(0), "未隔离时 inject 解析 root 绑定");
}

#[test]
fn inject_resolves_isolated_binding() {
    let ctx = Context::new();
    ctx.provide::<Db>(Arc::new(Db(0)));

    let r = ctx.alloc_realm();
    let scope = ctx.scoped(r).isolate_at::<Db>(r);
    scope.provide::<Db>(Arc::new(Db(7)));

    let seen = Arc::new(Mutex::new(None));
    scope.plugin(Arc::new(DbReader(Arc::clone(&seen))));
    assert_eq!(*seen.lock().unwrap(), Some(7), "隔离后 inject 解析 scope 绑定");
}

// —— 级联：严格判定 ——

#[test]
fn cascade_is_strict_on_isolated() {
    let ctx = Context::new();
    // root 也有 Db：若级联不是严格判定，scope 的 dependent 会「回退存活」——本测试钉死严格语义
    ctx.provide::<Db>(Arc::new(Db(0)));

    let r = ctx.alloc_realm();
    let scope = ctx.scoped(r).isolate_at::<Db>(r);

    struct Provider;
    impl Plugin for Provider {
        fn inject(&self) -> Vec<TypeId> {
            vec![]
        }
        fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
            ctx.provide::<Db>(Arc::new(Db(1)));
            Ok(())
        }
    }

    let cleaned = Arc::new(AtomicUsize::new(0));
    struct Dependent(Arc<AtomicUsize>);
    impl Plugin for Dependent {
        fn inject(&self) -> Vec<TypeId> {
            vec![TypeId::of::<Db>()]
        }
        fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
            let c = Arc::clone(&self.0);
            ctx.effect_sync(move || {
                c.fetch_add(1, Ordering::SeqCst);
            });
            Ok(())
        }
    }

    let provider = scope.plugin(Arc::new(Provider));
    let c = Arc::clone(&cleaned);
    let _dep = scope.plugin(Arc::new(Dependent(c)));

    assert_eq!(cleaned.load(Ordering::SeqCst), 0);

    // 卸载 provider → scope 的 Db(1) 撤销 → dependent 严格判定缺失 → 级联卸载
    // （即使 root 还有 Db(0)，dependent 的 realm 是 r，不回退）
    block_on(provider.dispose());
    assert_eq!(cleaned.load(Ordering::SeqCst), 1, "隔离 scope 的 dependent 被严格级联卸载");
    assert_eq!(ctx.get::<Db>().map(|d| d.0), Some(0), "root 绑定不受影响");
}
