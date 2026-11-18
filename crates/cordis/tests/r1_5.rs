//! R1.5 一致性测试：级联卸载时序精化（论文「withdrawal 提前可见」）。
//!
//! 模型（rust-port-design.md §4.3 时序精化 + Theorem 16 LIFO / Corollary 21 任意序）：
//! - provider 开始卸载即撤回绑定（值校验 revoke_if，只撤自己那份）——依赖者立即感知；
//! - 依赖者先执行逆（跨组件 LIFO：后 effect 先逆），再执行 provider 的逆；
//! - 值校验撤销：卸载窗口期内被重新 provide 的同类型新绑定不被误删。

use std::any::TypeId;
use std::sync::{Arc, Mutex};

use cordis::{Context, Plugin, PluginError, Service};

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

struct Db(u32);

impl Service for Db {
    fn name(&self) -> &'static str {
        "db"
    }
}

/// provider：provide Db(1) + 逆里记录顺序。
struct Provider {
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl Plugin for Provider {
    fn inject(&self) -> Vec<TypeId> {
        vec![]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        ctx.provide::<Db>(Arc::new(Db(1)));
        let order = Arc::clone(&self.order);
        ctx.effect_sync(move || order.lock().unwrap().push("provider"));
        Ok(())
    }
}

/// consumer：inject Db，逆里记录顺序并检查 Db 已不可见。
struct Consumer {
    order: Arc<Mutex<Vec<&'static str>>>,
    saw: Arc<Mutex<Vec<Option<u32>>>>,
}

impl Plugin for Consumer {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<Db>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let order = Arc::clone(&self.order);
        let saw = Arc::clone(&self.saw);
        let ctx2 = ctx.clone();
        ctx.effect_sync(move || {
            order.lock().unwrap().push("consumer");
            saw.lock().unwrap().push(ctx2.get::<Db>().map(|d| d.0));
        });
        Ok(())
    }
}

#[test]
fn dependent_inverse_runs_before_provider_inverse() {
    let ctx = Context::new();
    let order = Arc::new(Mutex::new(Vec::new()));
    let saw = Arc::new(Mutex::new(Vec::new()));

    let provider = ctx.plugin(Arc::new(Provider {
        order: Arc::clone(&order),
    }));
    let _consumer = ctx.plugin(Arc::new(Consumer {
        order: Arc::clone(&order),
        saw: Arc::clone(&saw),
    }));

    block_on(provider.dispose());

    assert_eq!(
        &*order.lock().unwrap(),
        &["consumer", "provider"],
        "依赖者逆先于 provider 逆（跨组件 LIFO）"
    );
    assert_eq!(
        &*saw.lock().unwrap(),
        &[None],
        "consumer 逆执行时依赖已被提前撤回（withdrawal 提前可见）"
    );
}

// —— 三层依赖链：C ← B ← A，逆序 C, B, A ——

struct S1;
impl Service for S1 {
    fn name(&self) -> &'static str {
        "s1"
    }
}
struct S2;
impl Service for S2 {
    fn name(&self) -> &'static str {
        "s2"
    }
}

struct Node {
    name: &'static str,
    inject: Vec<TypeId>,
    provides: Option<fn(&Context)>,
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl Plugin for Node {
    fn inject(&self) -> Vec<TypeId> {
        self.inject.clone()
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        if let Some(p) = self.provides {
            p(ctx);
        }
        let order = Arc::clone(&self.order);
        let name = self.name;
        ctx.effect_sync(move || order.lock().unwrap().push(name));
        Ok(())
    }
}

#[test]
fn chain_inverse_order_is_dependents_first() {
    let ctx = Context::new();
    let order = Arc::new(Mutex::new(Vec::new()));

    let mk = |name, inject: Vec<TypeId>, provides, order: &Arc<Mutex<Vec<&'static str>>>| {
        Node {
            name,
            inject,
            provides,
            order: Arc::clone(order),
        }
    };

    let a = ctx.plugin(Arc::new(mk(
        "a",
        vec![],
        Some(|c| c.provide::<S1>(Arc::new(S1))),
        &order,
    )));
    let _b = ctx.plugin(Arc::new(mk(
        "b",
        vec![TypeId::of::<S1>()],
        Some(|c| c.provide::<S2>(Arc::new(S2))),
        &order,
    )));
    let _c = ctx.plugin(Arc::new(mk("c", vec![TypeId::of::<S2>()], None, &order)));

    block_on(a.dispose());

    assert_eq!(
        &*order.lock().unwrap(),
        &["c", "b", "a"],
        "依赖链逆序：最下游先逆，provider 最后"
    );
}

#[test]
fn unload_does_not_revoke_reprovided_binding() {
    let ctx = Context::new();
    let order = Arc::new(Mutex::new(Vec::new()));
    let provider = ctx.plugin(Arc::new(Provider {
        order: Arc::clone(&order),
    }));

    // provider Active（提供了 Db(1)）后，root 覆盖提供新值
    ctx.provide::<Db>(Arc::new(Db(2)));

    block_on(provider.dispose());

    assert_eq!(
        ctx.get::<Db>().map(|d| d.0),
        Some(2),
        "卸载不误删重新 provide 的新绑定（值校验撤销）"
    );
    assert_eq!(&*order.lock().unwrap(), &["provider"]);
}

#[test]
fn failed_apply_leaves_no_provided_binding() {
    struct Broken;

    impl Plugin for Broken {
        fn inject(&self) -> Vec<TypeId> {
            vec![]
        }

        fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
            ctx.provide::<Db>(Arc::new(Db(9)));
            Err("boom".into())
        }
    }

    let ctx = Context::new();
    let handle = ctx.plugin(Arc::new(Broken));
    assert_eq!(
        handle.state(),
        cordis::FiberState::Failed,
        "apply 失败 → 进入 Failed（不因依赖刷新自动重入）"
    );
    assert!(
        ctx.get::<Db>().is_none(),
        "apply 失败后中途 provide 的绑定已撤回，不留残留"
    );
}
