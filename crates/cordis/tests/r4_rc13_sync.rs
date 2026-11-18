//! R4 一致性测试：同步上游 `cordiverse/cordis` `v4.0.0-rc.8` → `v4.0.0-rc.13` 全部核心语义。
//!
//! 覆盖范围：
//! 1. `Component` / `use_component` / `inject` / 嵌套组件与 `root.dispose()` / 失活上下文保护（`#307`, `#133`）；
//! 2. `FiberState::Failed` 非重入、半途失败全量回滚、`Idle`/`Failed` 卸载收敛至 `Disposed`（`#98`, `#109`, `#133`）；
//! 3. 响应式卸载恢复（`Idle -> Active -> Idle -> Active`）与服务提供者在全部消费者异步排空后才销毁（`#191`, `#196`, `#277`）；
//! 4. 5 种事件模式分发期动态移除即时跳过、新增推迟、`once` 重入/并发至多一次、`prepend`/`global`、空桶回收（`#44`, `#51`, `#242`, `#296`）；
//! 5. 显式 `isolate` / `isolate_at` 边界阻断外层 `locate` 与注入点拦截（`#206`, `#209`）；
//! 6. 配置校验、`on_update` 热更新钩子（不跨重载累积）、`restart` 与 `replace_component` 原地热替换及别名链（`#56`, `#134`, `#189`, `#266`）。

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cordis::{
    intercept_fn, BailEvent, BoxFuture, Component, ComponentError, ConfigurableComponent, Context,
    CordisErrorCode, Disposer, EmitEvent, EventOptions, FiberState, ParallelEvent, SerialEvent,
    Service, WaterfallEvent,
};
use futures::channel::oneshot;

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

// ==================== 公共测试服务与事件 ====================

struct Foo {
    version: u32,
    closed: Arc<AtomicBool>,
    timeline: Arc<Mutex<Vec<String>>>,
}

impl Foo {
    fn new(version: u32, closed: Arc<AtomicBool>, timeline: Arc<Mutex<Vec<String>>>) -> Arc<Self> {
        Arc::new(Self {
            version,
            closed,
            timeline,
        })
    }

    fn read(&self, tag: &str) {
        let state = if self.closed.load(Ordering::SeqCst) {
            "closed"
        } else {
            "open"
        };
        self.timeline
            .lock()
            .unwrap()
            .push(format!("{tag}:{state}:v{}", self.version));
    }
}

impl Service for Foo {
    fn name(&self) -> &'static str {
        "foo"
    }
}

struct Bar(u32);
impl Service for Bar {
    fn name(&self) -> &'static str {
        "bar"
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TaggedCfg {
    a: Option<u32>,
    b: Option<u32>,
    c: Option<u32>,
    tags: Vec<&'static str>,
}

impl Service for TaggedCfg {
    fn name(&self) -> &'static str {
        "tagged_cfg"
    }
}

struct EvEmit;
impl EmitEvent for EvEmit {
    type Args = u32;
}

struct EvBail;
impl BailEvent for EvBail {
    type Args = u32;
    type Ret = &'static str;
}

struct EvSerial;
impl SerialEvent for EvSerial {
    type Args = u32;
    type Ret = u32;
}

struct EvParallel;
impl ParallelEvent for EvParallel {
    type Args = u32;
}

struct EvWaterfall;
impl WaterfallEvent for EvWaterfall {
    type Args = u32;
    type Ret = u32;
}

// ==================== 1. Component、嵌套组件、root.dispose 与失活上下文 ====================

#[test]
fn nested_components_and_root_dispose_clean_up_all_levels() {
    let root = Context::new();
    let hits = Arc::new(AtomicUsize::new(0));

    let h0 = Arc::clone(&hits);
    root.on_emit_scoped::<EvEmit>(move |_| {
        h0.fetch_add(1, Ordering::SeqCst);
    });

    struct Level1(Arc<AtomicUsize>);
    impl Component for Level1 {
        fn name(&self) -> Option<&'static str> {
            Some("level1")
        }
        fn apply(&self, ctx: &Context) -> Result<(), ComponentError> {
            assert_eq!(format!("{ctx:?}"), "Context <level1>");
            let h1 = Arc::clone(&self.0);
            ctx.on_emit_scoped::<EvEmit>(move |_| {
                h1.fetch_add(1, Ordering::SeqCst);
            });
            let h2 = Arc::clone(&self.0);
            ctx.inject([], move |inner| {
                // 未命名子组件回退继承父组件名
                assert_eq!(format!("{inner:?}"), "Context <level1>");
                let h2c = Arc::clone(&h2);
                inner.on_emit_scoped::<EvEmit>(move |_| {
                    h2c.fetch_add(1, Ordering::SeqCst);
                });
                let h3 = Arc::clone(&h2);
                inner.inject([], move |leaf| {
                    let h3c = Arc::clone(&h3);
                    leaf.on_emit_scoped::<EvEmit>(move |_| {
                        h3c.fetch_add(1, Ordering::SeqCst);
                    });
                    Ok(())
                });
                Ok(())
            });
            Ok(())
        }
    }

    let handle = root.component(Level1(Arc::clone(&hits)));
    assert_eq!(root.registry_size(), 3);
    root.emit::<EvEmit>(1);
    assert_eq!(hits.load(Ordering::SeqCst), 4);

    // 卸载外层组件 → 三层子组件与各自监听器全部自动卸载，只剩 root 监听器
    block_on(handle.dispose());
    assert_eq!(root.registry_size(), 0);
    hits.store(0, Ordering::SeqCst);
    root.emit::<EvEmit>(2);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // 再次卸载是幂等空操作
    block_on(handle.dispose());

    // root.dispose() 清理根上挂载的组件与监听器，但根上下文自身保持活跃可复用
    let root_cleaned = Arc::new(AtomicUsize::new(0));
    let rc = Arc::clone(&root_cleaned);
    let child = root.inject([], move |c| {
        let rc2 = Arc::clone(&rc);
        c.effect_sync(move || {
            rc2.fetch_add(1, Ordering::SeqCst);
        });
        Ok(())
    });
    block_on(root.dispose());
    assert_eq!(child.state(), FiberState::Disposed);
    assert_eq!(root_cleaned.load(Ordering::SeqCst), 1);
    assert!(root.is_active(), "root.dispose 后根上下文仍保持活跃");
    block_on(root.dispose());
    assert_eq!(root_cleaned.load(Ordering::SeqCst), 1);
}

#[test]
fn inactive_context_rejects_new_effects_and_components() {
    let root = Context::new();
    let captured: Arc<Mutex<Option<Context>>> = Arc::new(Mutex::new(None));
    let cap = Arc::clone(&captured);

    let handle = root.inject([], move |ctx| {
        *cap.lock().unwrap() = Some(ctx.clone());
        let dead_ctx = ctx.clone();
        ctx.effect_sync(move || {
            assert!(!dead_ctx.is_active());
            let err = dead_ctx
                .try_effect_sync(|| {})
                .expect_err("失活上下文不得注册新 effect");
            assert_eq!(err.code, CordisErrorCode::InactiveEffect);
            assert!(dead_ctx.try_on_emit_scoped::<EvEmit>(|_| {}).is_err());
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dead_ctx.effect_sync(|| {});
                }))
                .is_err(),
                "非 try 入口在失活上下文上应直接拒绝"
            );
        });
        Ok(())
    });

    block_on(handle.dispose());
    let dead = captured.lock().unwrap().take().unwrap();
    assert!(!dead.is_active());
    assert!(dead.try_provide::<Bar>(Arc::new(Bar(1))).is_err());
}

// ==================== 2. Failed 状态机、完整回滚与收敛 ====================

#[test]
fn failed_fiber_rolls_back_partial_effects_and_does_not_reenter_on_refresh() {
    let root = Context::new();
    let p1 = root.provide_scoped::<Bar>(Arc::new(Bar(1)));

    let runs = Arc::new(AtomicUsize::new(0));
    let rolled_back = Arc::new(AtomicUsize::new(0));
    let leaked_event = Arc::new(AtomicUsize::new(0));

    let r = Arc::clone(&runs);
    let rb = Arc::clone(&rolled_back);
    let le = Arc::clone(&leaked_event);

    let fiber = root.inject([TypeId::of::<Bar>()], move |ctx| {
        r.fetch_add(1, Ordering::SeqCst);
        let rb2 = Arc::clone(&rb);
        ctx.effect_sync(move || {
            rb2.fetch_add(1, Ordering::SeqCst);
        });
        let le2 = Arc::clone(&le);
        ctx.on_emit_scoped::<EvEmit>(move |_| {
            le2.fetch_add(1, Ordering::SeqCst);
        });
        ctx.provide::<TaggedCfg>(Arc::new(TaggedCfg::default()));
        Err("boom".into())
    });

    assert_eq!(fiber.state(), FiberState::Failed);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        rolled_back.load(Ordering::SeqCst),
        1,
        "失败前已注册的 effect_sync 必须立即回滚"
    );
    assert!(
        root.get::<TaggedCfg>().is_none(),
        "失败前 provide 的服务必须撤回"
    );
    root.emit::<EvEmit>(42);
    assert_eq!(
        leaked_event.load(Ordering::SeqCst),
        0,
        "失败前注册的 scoped 事件监听器必须回滚移除"
    );

    // 撤回并重新提供依赖 Bar：Failed 组件不得自动重入（#98）
    block_on(p1.withdraw());
    root.provide::<Bar>(Arc::new(Bar(2)));
    assert_eq!(runs.load(Ordering::SeqCst), 1, "Failed 组件不得因依赖刷新重入");
    assert_eq!(fiber.state(), FiberState::Failed);

    // 卸载 Failed 与 Pending 组件必须收敛至 Disposed（#133）
    struct NeverProvided;
    impl Service for NeverProvided {
        fn name(&self) -> &'static str {
            "never"
        }
    }
    let pending = root.inject([TypeId::of::<NeverProvided>()], |_| Ok(()));
    assert_eq!(pending.state(), FiberState::PENDING);

    block_on(pending.dispose());
    block_on(fiber.dispose());
    assert_eq!(pending.state(), FiberState::Disposed);
    assert_eq!(fiber.state(), FiberState::Disposed);
}

// ==================== 3. 响应式重激活与 Provider-After-Dependent 异步卸载 ====================

#[test]
fn reactive_unload_returns_to_idle_and_reactivates_when_reprovided() {
    let root = Context::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);

    let consumer = root.inject([TypeId::of::<Bar>()], move |ctx| {
        let v = ctx.locate::<Bar>().expect("committed bar").0;
        s.lock().unwrap().push(format!("start:{v}"));
        let s_stop = Arc::clone(&s);
        ctx.effect_sync(move || {
            s_stop.lock().unwrap().push(format!("stop:{v}"));
        });
        Ok(())
    });
    assert_eq!(consumer.state(), FiberState::Idle);

    let p1 = root.provide_scoped::<Bar>(Arc::new(Bar(1)));
    assert_eq!(consumer.state(), FiberState::Active);

    // 撤回 Bar(1)：consumer 响应式卸载回 Idle（而非永久 Disposed！）
    block_on(p1.withdraw());
    assert_eq!(consumer.state(), FiberState::Idle);

    // 重新提供 Bar(2)：consumer 自动重新激活！
    let p2 = root.provide_scoped::<Bar>(Arc::new(Bar(2)));
    assert_eq!(consumer.state(), FiberState::Active);

    // 撤回 Bar(2) 后再次通过普通 provide 提供 Bar(3)：consumer 再次从 Idle 自动激活到 Bar(3)！
    block_on(p2.withdraw());
    assert_eq!(consumer.state(), FiberState::Idle);
    root.provide::<Bar>(Arc::new(Bar(3)));
    assert_eq!(consumer.state(), FiberState::Active);

    assert_eq!(
        *seen.lock().unwrap(),
        vec!["start:1", "stop:1", "start:2", "stop:2", "start:3"]
    );
}

#[test]
fn retains_provider_resources_during_async_consumer_cleanup_all_orders() {
    for mode in ["provider", "root", "consumer_first", "provider_first", "restart"] {
        let root = Context::new();
        let closed = Arc::new(AtomicBool::new(false));
        let timeline = Arc::new(Mutex::new(Vec::new()));
        let (gate_tx, gate_rx) = oneshot::channel::<()>();
        let gate_slot = Arc::new(Mutex::new(Some(gate_rx)));

        let closed_p = Arc::clone(&closed);
        let tl_p = Arc::clone(&timeline);
        let ver = Arc::new(AtomicUsize::new(0));
        let ver_p = Arc::clone(&ver);
        let provider = root.inject([], move |ctx| {
            let v = ver_p.fetch_add(1, Ordering::SeqCst) as u32 + 1;
            closed_p.store(false, Ordering::SeqCst);
            let foo = Foo::new(v, Arc::clone(&closed_p), Arc::clone(&tl_p));
            ctx.provide::<Foo>(foo);
            let c_close = Arc::clone(&closed_p);
            let tl_close = Arc::clone(&tl_p);
            ctx.effect_sync(move || {
                c_close.store(true, Ordering::SeqCst);
                tl_close.lock().unwrap().push(format!("provider_closed:v{v}"));
            });
            Ok(())
        });

        let gs = Arc::clone(&gate_slot);
        let expect_withdrawn = mode != "root";
        let consumer = root.inject([TypeId::of::<Foo>()], move |ctx| {
            if let Some(rx) = gs.lock().unwrap().take() {
                let cctx = ctx.clone();
                ctx.effect_async(async move {
                    let _ = rx.await;
                    if expect_withdrawn {
                        // provider 已进入卸载：strict get 立即感知撤回（None），但 locate 仍持有未关闭的本轮已提交实例！
                        assert!(cctx.get::<Foo>().is_none());
                    }
                    cctx.locate::<Foo>()
                        .expect("committed Foo must stay accessible during async cleanup")
                        .read("consumer");
                });
            }
            Ok(())
        });

        // 启动卸载任务并先轮询一次使其停在 gate_rx.await，验证此刻 provider 尚未关闭！
        let mut task: BoxFuture<'_, ()> = match mode {
            "root" => Box::pin(async { root.dispose().await }),
            "restart" => Box::pin(async {
                provider.restart().await.unwrap();
            }),
            "consumer_first" => Box::pin(async {
                futures::future::join(consumer.dispose(), provider.dispose()).await;
            }),
            "provider_first" => Box::pin(async {
                futures::future::join(provider.dispose(), consumer.dispose()).await;
            }),
            _ => Box::pin(async { provider.dispose().await }),
        };

        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(
            task.as_mut().poll(&mut cx).is_pending(),
            "mode={mode}: consumer 尚未放行，卸载不得提前完成"
        );
        assert!(
            !closed.load(Ordering::SeqCst),
            "mode={mode}: consumer 异步排空期间 provider 底层资源必须保持 open"
        );

        let _ = gate_tx.send(());
        block_on(task);

        assert_eq!(
            *timeline.lock().unwrap(),
            vec!["consumer:open:v1".to_string(), "provider_closed:v1".to_string()],
            "mode={mode}"
        );
        if mode == "restart" {
            assert_eq!(consumer.state(), FiberState::Active);
            assert_eq!(consumer.context().locate::<Foo>().unwrap().version, 2);
        }
    }
}

#[test]
fn transitive_chain_and_ancestor_injection_and_provider_listeners_stay_alive() {
    // (a) 传递依赖链 Foo -> Middleware(Bar) -> Leaf 异步排空
    {
        let root = Context::new();
        let closed = Arc::new(AtomicBool::new(false));
        let timeline = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = oneshot::channel::<()>();
        let rx_slot = Arc::new(Mutex::new(Some(rx)));

        let closed_p = Arc::clone(&closed);
        let tl_p = Arc::clone(&timeline);
        let owner = root.inject([], move |ctx| {
            let cp = Arc::clone(&closed_p);
            let tp = Arc::clone(&tl_p);
            ctx.inject([], move |pctx| {
                pctx.provide::<Foo>(Foo::new(1, Arc::clone(&cp), Arc::clone(&tp)));
                let cp2 = Arc::clone(&cp);
                let tp2 = Arc::clone(&tp);
                pctx.effect_sync(move || {
                    cp2.store(true, Ordering::SeqCst);
                    tp2.lock().unwrap().push("provider_closed".into());
                });
                Ok(())
            });

            ctx.inject([TypeId::of::<Foo>()], move |mctx| {
                mctx.provide::<Bar>(Arc::new(Bar(1)));
                let mctx2 = mctx.clone();
                mctx.effect_sync(move || {
                    mctx2.locate::<Foo>().unwrap().read("middleware");
                });
                Ok(())
            });

            let rxs = Arc::clone(&rx_slot);
            let tl_leaf = Arc::clone(&tl_p);
            ctx.inject([TypeId::of::<Bar>()], move |lctx| {
                let rx = rxs.lock().unwrap().take().unwrap();
                let tl_leaf2 = Arc::clone(&tl_leaf);
                lctx.effect_async(async move {
                    tl_leaf2.lock().unwrap().push("leaf_draining".into());
                    let _ = rx.await;
                    tl_leaf2.lock().unwrap().push("leaf_finished".into());
                });
                Ok(())
            });
            Ok(())
        });

        let mut fut: BoxFuture<'static, ()> = Box::pin(async move { owner.dispose().await });
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        assert!(!closed.load(Ordering::SeqCst));

        let _ = tx.send(());
        block_on(fut);
        assert_eq!(
            *timeline.lock().unwrap(),
            vec![
                "leaf_draining",
                "leaf_finished",
                "middleware:open:v1",
                "provider_closed"
            ]
        );
    }

    // (b) 消费者异步清理发出的最后一条事件仍能被提供者的 scoped 监听器收到（service-dispose.spec.ts:199）
    //     以及子组件显式 inject 父组件自身服务时不死锁（service-dispose.spec.ts:307）
    {
        let root = Context::new();
        let final_events = Arc::new(AtomicUsize::new(0));
        let fe = Arc::clone(&final_events);
        let child_read_open = Arc::new(AtomicBool::new(false));
        let cro = Arc::clone(&child_read_open);

        let provider = root.inject([], move |ctx| {
            let closed = Arc::new(AtomicBool::new(false));
            let tl = Arc::new(Mutex::new(Vec::new()));
            ctx.provide::<Foo>(Foo::new(1, Arc::clone(&closed), tl));
            let fe2 = Arc::clone(&fe);
            ctx.on_emit_scoped::<EvEmit>(move |_| {
                fe2.fetch_add(1, Ordering::SeqCst);
            });
            let c2 = Arc::clone(&closed);
            ctx.effect_sync(move || {
                c2.store(true, Ordering::SeqCst);
            });

            // 子组件显式 inject 父组件自身提供的 Foo
            let cro2 = Arc::clone(&cro);
            ctx.inject([TypeId::of::<Foo>()], move |child_ctx| {
                let cctx = child_ctx.clone();
                let cro3 = Arc::clone(&cro2);
                child_ctx.effect_async(async move {
                    let f = cctx.locate::<Foo>().unwrap();
                    cro3.store(!f.closed.load(Ordering::SeqCst), Ordering::SeqCst);
                    cctx.emit::<EvEmit>(99);
                });
                Ok(())
            });
            Ok(())
        });

        block_on(provider.dispose());
        assert!(child_read_open.load(Ordering::SeqCst));
        assert_eq!(final_events.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn panicking_consumer_disposer_still_releases_provider_dependents() {
    let root = Context::new();
    let closed = Arc::new(AtomicBool::new(false));
    let tl = Arc::new(Mutex::new(Vec::new()));
    let cp = Arc::clone(&closed);
    let tp = Arc::clone(&tl);

    let provider = root.inject([], move |ctx| {
        ctx.provide::<Foo>(Foo::new(1, Arc::clone(&cp), Arc::clone(&tp)));
        let cp2 = Arc::clone(&cp);
        ctx.effect_sync(move || {
            cp2.store(true, Ordering::SeqCst);
        });
        Ok(())
    });

    let consumer = root.inject([TypeId::of::<Foo>()], |ctx| {
        ctx.effect_sync(|| panic!("consumer cleanup panic"));
        Ok(())
    });

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        block_on(consumer.dispose());
    }));
    assert!(unwind.is_err());

    // 即使消费者清理 panic，UnloadGuard 仍已释放 Foo.dependents，provider.dispose() 不会死锁！
    block_on(provider.dispose());
    assert!(closed.load(Ordering::SeqCst));
}

// ==================== 4. 事件分发期移除、once、prepend/global 与空桶清理 ====================

#[test]
fn event_dispatch_skips_removed_listeners_and_defers_added_across_all_modes() {
    // 1. emit
    {
        let ctx = Context::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        let off_slot: Arc<Mutex<Option<Disposer>>> = Arc::new(Mutex::new(None));

        let l1 = Arc::clone(&log);
        let os = Arc::clone(&off_slot);
        let ctx_add = ctx.clone();
        let l_added = Arc::clone(&log);
        ctx.on_emit::<EvEmit>(move |_| {
            l1.lock().unwrap().push("first");
            if let Some(Disposer::Sync(f)) = os.lock().unwrap().take() {
                f();
            }
            let la = Arc::clone(&l_added);
            ctx_add.on_emit::<EvEmit>(move |_| la.lock().unwrap().push("added"));
        });
        let l_rem = Arc::clone(&log);
        *off_slot.lock().unwrap() = Some(ctx.on_emit::<EvEmit>(move |_| {
            l_rem.lock().unwrap().push("removed");
        }));
        let l_rem2 = Arc::clone(&log);
        ctx.on_emit::<EvEmit>(move |_| l_rem2.lock().unwrap().push("remaining"));

        ctx.emit::<EvEmit>(1);
        assert_eq!(*log.lock().unwrap(), vec!["first", "remaining"]);
        ctx.emit::<EvEmit>(2);
        assert_eq!(
            *log.lock().unwrap(),
            vec!["first", "remaining", "first", "remaining", "added"]
        );
    }

    // 2. bail
    {
        let ctx = Context::new();
        let off_slot: Arc<Mutex<Option<Disposer>>> = Arc::new(Mutex::new(None));
        let os = Arc::clone(&off_slot);
        ctx.on_bail::<EvBail>(move |_| {
            if let Some(Disposer::Sync(f)) = os.lock().unwrap().take() {
                f();
            }
            None
        });
        *off_slot.lock().unwrap() = Some(ctx.on_bail::<EvBail>(|_| Some("removed")));
        ctx.on_bail::<EvBail>(|_| Some("remaining"));
        assert_eq!(ctx.bail::<EvBail>(1), Some("remaining"));
    }

    // 3. serial（含通过卸载子组件移除下游监听器，events.spec.ts:188）
    {
        let ctx = Context::new();
        let child_slot: Arc<Mutex<Option<cordis::FiberHandle>>> = Arc::new(Mutex::new(None));
        let cs = Arc::clone(&child_slot);
        ctx.on_serial::<EvSerial>(move |_| {
            let h = cs.lock().unwrap().take();
            Box::pin(async move {
                if let Some(handle) = h {
                    handle.dispose().await;
                }
                None
            })
        });
        let child = ctx.inject([], |c| {
            c.on_serial_scoped::<EvSerial>(|_| Box::pin(async { Some(999) }));
            Ok(())
        });
        *child_slot.lock().unwrap() = Some(child);
        ctx.on_serial::<EvSerial>(|_| Box::pin(async { Some(7) }));

        assert_eq!(block_on(ctx.serial::<EvSerial>(1)), Some(7));
    }

    // 4. parallel
    {
        let ctx = Context::new();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let off_slot: Arc<Mutex<Option<Disposer>>> = Arc::new(Mutex::new(None));
        let os = Arc::clone(&off_slot);
        let h1 = Arc::clone(&hits);
        ctx.on_parallel::<EvParallel>(move |_| {
            if let Some(Disposer::Sync(f)) = os.lock().unwrap().take() {
                f();
            }
            let h = Arc::clone(&h1);
            Box::pin(async move { h.lock().unwrap().push("first") })
        });
        let h2 = Arc::clone(&hits);
        *off_slot.lock().unwrap() = Some(ctx.on_parallel::<EvParallel>(move |_| {
            let h = Arc::clone(&h2);
            Box::pin(async move { h.lock().unwrap().push("removed") })
        }));
        let h3 = Arc::clone(&hits);
        ctx.on_parallel::<EvParallel>(move |_| {
            let h = Arc::clone(&h3);
            Box::pin(async move { h.lock().unwrap().push("remaining") })
        });

        block_on(ctx.parallel::<EvParallel>(1));
        assert_eq!(*hits.lock().unwrap(), vec!["first", "remaining"]);
    }

    // 5. waterfall
    {
        let ctx = Context::new();
        let off_slot: Arc<Mutex<Option<Disposer>>> = Arc::new(Mutex::new(None));
        let os = Arc::clone(&off_slot);
        ctx.on_waterfall::<EvWaterfall>(move |v, next| {
            if let Some(Disposer::Sync(f)) = os.lock().unwrap().take() {
                f();
            }
            Box::pin(async move { v + next().await })
        });
        *off_slot.lock().unwrap() = Some(ctx.on_waterfall::<EvWaterfall>(|v, next| {
            Box::pin(async move { v * 100 + next().await })
        }));
        ctx.on_waterfall::<EvWaterfall>(|v, next| Box::pin(async move { v + next().await }));

        let out = block_on(ctx.waterfall::<EvWaterfall, _>(1, |_| Box::pin(async { 2 })));
        assert_eq!(out, 4, "被移除的中间层跳过，1 + (1 + 2) = 4");
    }
}

#[test]
fn event_once_prepend_global_and_empty_bucket_cleanup() {
    // 空桶清理（#51）
    let ctx = Context::new();
    assert!(!ctx.has_event_bucket::<EvEmit>());
    let d1 = ctx.on_emit::<EvEmit>(|_| {});
    let d2 = ctx.on_emit::<EvEmit>(|_| {});
    assert!(ctx.has_event_bucket::<EvEmit>());
    block_on(d1.run());
    assert!(ctx.has_event_bucket::<EvEmit>());
    block_on(d2.run());
    assert!(
        !ctx.has_event_bucket::<EvEmit>(),
        "所有监听器移除后空桶必须清理"
    );

    // once 在递归 emit 与递归 waterfall 下只触发一次（#296, events.spec.ts:134, 233）
    let nested = Arc::new(AtomicBool::new(false));
    let n = Arc::clone(&nested);
    let c_rec = ctx.clone();
    ctx.on_emit::<EvEmit>(move |_| {
        if !n.swap(true, Ordering::SeqCst) {
            c_rec.emit::<EvEmit>(2);
        }
    });
    let once_hits = Arc::new(AtomicUsize::new(0));
    let oh = Arc::clone(&once_hits);
    ctx.on_emit_once::<EvEmit>(move |_| {
        oh.fetch_add(1, Ordering::SeqCst);
    });
    ctx.emit::<EvEmit>(1);
    assert_eq!(once_hits.load(Ordering::SeqCst), 1);

    let wf_nested = Arc::new(AtomicBool::new(false));
    let wn = Arc::clone(&wf_nested);
    let c_wf = ctx.clone();
    ctx.on_waterfall::<EvWaterfall>(move |v, next| {
        let c = c_wf.clone();
        let wn2 = Arc::clone(&wn);
        Box::pin(async move {
            if !wn2.swap(true, Ordering::SeqCst) {
                let _ = c.waterfall::<EvWaterfall, _>(v, |_| Box::pin(async { 2 })).await;
            }
            next().await
        })
    });
    let wf_once_hits = Arc::new(AtomicUsize::new(0));
    let woh = Arc::clone(&wf_once_hits);
    ctx.on_waterfall_once::<EvWaterfall>(move |v, next| {
        let woh2 = Arc::clone(&woh);
        Box::pin(async move {
            woh2.fetch_add(1, Ordering::SeqCst);
            v + next().await
        })
    });
    ctx.on_waterfall::<EvWaterfall>(|v, next| Box::pin(async move { v + next().await }));
    let res = block_on(ctx.waterfall::<EvWaterfall, _>(1, |_| Box::pin(async { 2 })));
    assert_eq!(res, 3, "外层 waterfall 跳过已被递归消费的 once 监听器");
    assert_eq!(wf_once_hits.load(Ordering::SeqCst), 1);

    // prepend + global 选项
    let ctx2 = Context::new();
    let order = Arc::new(Mutex::new(Vec::new()));
    let scope_a = ctx2.scoped(ctx2.alloc_realm());
    let scope_b = ctx2.scoped(ctx2.alloc_realm());
    let o1 = Arc::clone(&order);
    scope_a.on_emit_with::<EvEmit>(
        EventOptions {
            prepend: false,
            global: true,
        },
        move |_| o1.lock().unwrap().push("global_from_a"),
    );
    let o2 = Arc::clone(&order);
    scope_b.on_emit_with::<EvEmit>(
        EventOptions {
            prepend: true,
            global: false,
        },
        move |_| o2.lock().unwrap().push("prepended_b"),
    );
    scope_b.emit::<EvEmit>(1);
    assert_eq!(
        *order.lock().unwrap(),
        vec!["prepended_b", "global_from_a"]
    );
}

// ==================== 5. 隔离边界与注入点拦截元数据（#206, #209） ====================

#[test]
fn isolation_boundary_stops_locate_and_inject_intercept_inheritance() {
    let root = Context::new();
    root.provide::<TaggedCfg>(Arc::new(TaggedCfg::default()));

    let results = Arc::new(Mutex::new(Vec::new()));
    let res = Arc::clone(&results);

    // 声明注入点拦截：{ a: 1, b: 1, tags: ["inject"] }
    let mut inj_map = HashMap::new();
    inj_map.insert(
        TypeId::of::<TaggedCfg>(),
        intercept_fn::<TaggedCfg>(|base| {
            let mut next = (*base).clone();
            next.a = Some(1);
            next.b = Some(1);
            next.tags.push("inject");
            Arc::new(next)
        }),
    );

    let outer = root.intercept::<TaggedCfg>(|base| {
        let mut next = (*base).clone();
        next.b = Some(2);
        next.tags.push("outer");
        Arc::new(next)
    });

    outer.inject_with_intercepts([TypeId::of::<TaggedCfg>()], inj_map, move |ctx| {
        // 1. 顺序：inject < outer < inner（intercept.spec.ts:42, 103）
        let merged = ctx
            .intercept::<TaggedCfg>(|base| {
                let mut next = (*base).clone();
                next.c = Some(3);
                next.tags.push("inner");
                Arc::new(next)
            })
            .locate::<TaggedCfg>()
            .unwrap();
        assert_eq!(merged.a, Some(1));
        assert_eq!(merged.b, Some(2));
        assert_eq!(merged.c, Some(3));
        assert_eq!(merged.tags, vec!["inject", "outer", "inner"]);

        // 2. 未声明 inject 的子组件继承父组件的 committed 视图与注入拦截（intercept.spec.ts:58）
        let r1 = Arc::clone(&res);
        ctx.inject([], move |child| {
            r1.lock()
                .unwrap()
                .push(child.locate::<TaggedCfg>().map(|c| c.a));
            Ok(())
        });

        // 3. 重新声明 inject 的子组件采用最近声明（不合并外层 inject 拦截，intercept.spec.ts:62）
        let mut child_inj = HashMap::new();
        child_inj.insert(
            TypeId::of::<TaggedCfg>(),
            intercept_fn::<TaggedCfg>(|base| {
                let mut next = (*base).clone();
                next.c = Some(9);
                Arc::new(next)
            }),
        );
        let r2 = Arc::clone(&res);
        ctx.inject_with_intercepts([TypeId::of::<TaggedCfg>()], child_inj, move |child| {
            let got = child.locate::<TaggedCfg>().unwrap();
            assert_eq!(got.a, None, "外层 inject 拦截不应混入内层重声明");
            assert_eq!(got.c, Some(9));
            r2.lock().unwrap().push(Some(got.c));
            Ok(())
        });

        // 4. 隔离边界阻断 locate，即便显式 pin 相同 ROOT realm（#209, isolate.spec.ts:43）
        let r3 = Arc::clone(&res);
        ctx.isolate::<TaggedCfg>().inject([], move |iso_child| {
            r3.lock()
                .unwrap()
                .push(iso_child.locate::<TaggedCfg>().map(|c| c.a));
            Ok(())
        });
        let r4 = Arc::clone(&res);
        ctx.isolate_at::<TaggedCfg>(cordis::RealmId::ROOT)
            .inject([], move |pinned_child| {
                r4.lock()
                    .unwrap()
                    .push(pinned_child.locate::<TaggedCfg>().map(|c| c.a));
                Ok(())
            });

        // 5. 组件自身在隔离域中 provide 的服务不受本组件对外部依赖的 inject 拦截影响（intercept.spec.ts:83）
        let own_iso = ctx.isolate::<TaggedCfg>();
        own_iso.provide::<TaggedCfg>(Arc::new(TaggedCfg::default()));
        let own_cfg = own_iso.locate::<TaggedCfg>().unwrap();
        assert_eq!(
            own_cfg.a, None,
            "自身 provide 的服务不得套用针对外部依赖的 inject 拦截"
        );
        Ok(())
    });

    assert_eq!(
        *results.lock().unwrap(),
        vec![Some(Some(1)), Some(Some(9)), None, None]
    );
}

// ==================== 6. 配置校验、on_update 热更新与 replace_component ====================

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkerCfg {
    ok: bool,
    n: u32,
    normalized: bool,
}

struct ConfigurableWorker {
    applied: Arc<Mutex<Vec<WorkerCfg>>>,
    updated: Arc<Mutex<Vec<WorkerCfg>>>,
    veto_restart: bool,
}

impl ConfigurableComponent for ConfigurableWorker {
    type Config = WorkerCfg;

    fn validate(&self, mut config: Self::Config) -> Result<Self::Config, ComponentError> {
        if !config.ok {
            return Err("invalid worker config".into());
        }
        config.normalized = true;
        Ok(config)
    }

    fn apply(&self, ctx: &Context, config: Self::Config) -> Result<(), ComponentError> {
        self.applied.lock().unwrap().push(config);
        let updated = Arc::clone(&self.updated);
        let veto = self.veto_restart;
        ctx.on_update::<WorkerCfg>(move |cfg, next| {
            updated.lock().unwrap().push(cfg);
            if veto {
                Box::pin(async {})
            } else {
                next()
            }
        });
        Ok(())
    }
}

#[test]
fn config_validation_and_on_update_lifecycle() {
    let root = Context::new();
    let applied = Arc::new(Mutex::new(Vec::new()));
    let updated = Arc::new(Mutex::new(Vec::new()));

    let worker = Arc::new(ConfigurableWorker {
        applied: Arc::clone(&applied),
        updated: Arc::clone(&updated),
        veto_restart: false,
    });

    // 首次加载配置非法 → Failed 且 apply 不执行（fiber.spec.ts:384）
    let bad = root.use_with(
        Arc::clone(&worker),
        WorkerCfg {
            ok: false,
            n: 0,
            normalized: false,
        },
    );
    assert_eq!(bad.state(), FiberState::Failed);
    assert!(applied.lock().unwrap().is_empty());

    // update 合法配置可恢复 Failed fiber（fiber.spec.ts:108）
    block_on(bad.update(WorkerCfg {
        ok: true,
        n: 1,
        normalized: false,
    }))
    .expect("合法配置应恢复 Failed 组件");
    assert_eq!(bad.state(), FiberState::Active);
    assert_eq!(applied.lock().unwrap().len(), 1);
    assert!(applied.lock().unwrap()[0].normalized);

    // 活跃态传入非法配置 → update 报错，组件保持 Active 且保留旧配置（fiber.spec.ts:396）
    assert!(block_on(bad.update(WorkerCfg {
        ok: false,
        n: 99,
        normalized: false,
    }))
    .is_err());
    assert_eq!(bad.state(), FiberState::Active);
    assert_eq!(bad.config::<WorkerCfg>().unwrap().n, 1);

    // 多次连续 update：on_update 钩子随重载清理，绝不跨轮次重复累积（#56, events.spec.ts:473）
    for n in [2, 3, 4] {
        block_on(bad.update(WorkerCfg {
            ok: true,
            n,
            normalized: false,
        }))
        .unwrap();
    }
    let upd_ns: Vec<u32> = updated.lock().unwrap().iter().map(|c| c.n).collect();
    assert_eq!(upd_ns, vec![2, 3, 4]);
    // 源配置保留未 normalized 版本（fiber.spec.ts:448）
    assert_eq!(
        bad.config::<WorkerCfg>().unwrap(),
        WorkerCfg {
            ok: true,
            n: 4,
            normalized: false,
        }
    );

    // on_update 拦截（不调用 next）时热消化配置而不触发重启
    let applied2 = Arc::new(Mutex::new(Vec::new()));
    let updated2 = Arc::new(Mutex::new(Vec::new()));
    let hot_worker = Arc::new(ConfigurableWorker {
        applied: Arc::clone(&applied2),
        updated: Arc::clone(&updated2),
        veto_restart: true,
    });
    let hot = root.use_with(
        hot_worker,
        WorkerCfg {
            ok: true,
            n: 10,
            normalized: false,
        },
    );
    block_on(hot.update(WorkerCfg {
        ok: true,
        n: 20,
        normalized: false,
    }))
    .unwrap();
    assert_eq!(applied2.lock().unwrap().len(), 1, "拦截更新不应重启 apply");
    assert_eq!(updated2.lock().unwrap()[0].n, 20);
    block_on(hot.restart()).unwrap();
    assert_eq!(applied2.lock().unwrap().len(), 2, "随后显式 restart 应用保存的源配置");
    assert_eq!(applied2.lock().unwrap()[1].n, 20);
}

#[test]
fn replace_component_in_place_aliases_deps_and_consumers() {
    let root = Context::new();
    let log = Arc::new(Mutex::new(Vec::new()));

    let l1 = Arc::clone(&log);
    let old_prov: Arc<dyn Component> = Arc::new(FnComp(
        vec![],
        Box::new(move |ctx| {
            ctx.provide::<Bar>(Arc::new(Bar(10)));
            let lc = Arc::clone(&l1);
            ctx.effect_sync(move || lc.lock().unwrap().push("dispose_old"));
            Ok(())
        }),
    ));

    let l2 = Arc::clone(&log);
    let new_prov: Arc<dyn Component> = Arc::new(FnComp(
        vec![],
        Box::new(move |ctx| {
            ctx.provide::<Bar>(Arc::new(Bar(20)));
            let lc = Arc::clone(&l2);
            ctx.effect_sync(move || lc.lock().unwrap().push("dispose_new"));
            Ok(())
        }),
    ));

    let seen_bars = Arc::new(Mutex::new(Vec::new()));
    let sb = Arc::clone(&seen_bars);
    let consumer = root.inject([TypeId::of::<Bar>()], move |ctx| {
        sb.lock()
            .unwrap()
            .push(ctx.locate::<Bar>().unwrap().0);
        Ok(())
    });

    let handle = root.use_component(Arc::clone(&old_prov));
    let orig_id = handle.id();
    assert_eq!(*seen_bars.lock().unwrap(), vec![10]);

    // 1. 原地替换服务提供者 → 旧逆执行、新服务生效、下游 consumer 自动重载且句柄 ID 不变（replace.spec.ts:7, 121）
    let replaced = root.replace_component(&old_prov, Arc::clone(&new_prov));
    assert_eq!(replaced.len(), 1);
    block_on(handle.await_ready()).unwrap();
    assert_eq!(handle.id(), orig_id);
    assert_eq!(handle.state(), FiberState::Active);
    assert_eq!(consumer.state(), FiberState::Active);
    assert_eq!(*log.lock().unwrap(), vec!["dispose_old"]);
    assert_eq!(*seen_bars.lock().unwrap(), vec![10, 20]);

    // 2. 别名转发与链式替换 + 切回（replace.spec.ts:186, 220, 234）
    let hits_a = Arc::new(AtomicUsize::new(0));
    let hits_b = Arc::new(AtomicUsize::new(0));
    let hits_c = Arc::new(AtomicUsize::new(0));
    let ha = Arc::clone(&hits_a);
    let hb = Arc::clone(&hits_b);
    let hc = Arc::clone(&hits_c);
    let comp_a: Arc<dyn Component> = Arc::new(FnComp(
        vec![],
        Box::new(move |_| {
            ha.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
    ));
    let comp_b: Arc<dyn Component> = Arc::new(FnComp(
        vec![],
        Box::new(move |_| {
            hb.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
    ));
    let comp_c: Arc<dyn Component> = Arc::new(FnComp(
        vec![TypeId::of::<TaggedCfg>()],
        Box::new(move |_| {
            hc.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
    ));

    let fa = root.use_component(Arc::clone(&comp_a));
    assert_eq!(hits_a.load(Ordering::SeqCst), 1);

    root.replace_component(&comp_a, Arc::clone(&comp_b));
    assert_eq!(hits_b.load(Ordering::SeqCst), 1);

    // 替换为带缺失依赖 TaggedCfg 的 comp_c → fa 卸载并进入 Idle（replace.spec.ts:60）
    root.replace_component(&comp_a, Arc::clone(&comp_c));
    assert_eq!(fa.state(), FiberState::Idle);
    assert_eq!(hits_c.load(Ordering::SeqCst), 0);

    // 通过旧引用 comp_a 再次挂载会自动转发到 comp_c
    let fa2 = root.use_component(Arc::clone(&comp_a));
    assert_eq!(fa2.state(), FiberState::Idle);

    // 提供 TaggedCfg 后，fa 与 fa2 全部激活运行 comp_c
    let prov_cfg = root.provide_scoped::<TaggedCfg>(Arc::new(TaggedCfg::default()));
    assert_eq!(fa.state(), FiberState::Active);
    assert_eq!(fa2.state(), FiberState::Active);
    assert_eq!(hits_c.load(Ordering::SeqCst), 2);

    // 从 comp_c 切回无依赖的 comp_a（解除旧别名并脱离 TaggedCfg 依赖，replace.spec.ts:106, 234）
    root.replace_component(&comp_c, Arc::clone(&comp_a));
    assert_eq!(hits_a.load(Ordering::SeqCst), 3);
    block_on(prov_cfg.withdraw());
    assert_eq!(fa.state(), FiberState::Active, "已放弃依赖，撤回不再触发卸载");

    // 全部实例卸载后从注册表移除（replace.spec.ts:248）
    block_on(fa.dispose());
    block_on(fa2.dispose());
    assert!(!root.has_component(&comp_a));
    assert!(!root.has_component(&comp_c));
}

type ApplyCallback = Box<dyn Fn(&Context) -> Result<(), ComponentError> + Send + Sync>;

struct FnComp(Vec<TypeId>, ApplyCallback);

impl Component for FnComp {
    fn inject(&self) -> Vec<TypeId> {
        self.0.clone()
    }
    fn apply(&self, ctx: &Context) -> Result<(), ComponentError> {
        (self.1)(ctx)
    }
}

#[test]
fn once_runs_once_across_concurrent_serial_dispatches() {
    let ctx = Context::new();
    let (tx1, rx1) = oneshot::channel::<()>();
    let (tx2, rx2) = oneshot::channel::<()>();
    let rx_list = Arc::new(Mutex::new(vec![rx2, rx1]));

    let rxs = Arc::clone(&rx_list);
    ctx.on_serial::<EvSerial>(move |_| {
        let rx = rxs.lock().unwrap().pop().unwrap();
        Box::pin(async move {
            let _ = rx.await;
            None
        })
    });

    let once_calls = Arc::new(AtomicUsize::new(0));
    let oc = Arc::clone(&once_calls);
    ctx.on_serial_once::<EvSerial>(move |v| {
        let oc2 = Arc::clone(&oc);
        Box::pin(async move {
            oc2.fetch_add(1, Ordering::SeqCst);
            Some(v * 10)
        })
    });

    let mut f1: BoxFuture<'_, Option<u32>> = Box::pin(ctx.serial::<EvSerial>(1));
    let mut f2: BoxFuture<'_, Option<u32>> = Box::pin(ctx.serial::<EvSerial>(2));
    let waker = futures::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    // 两次并发 serial 都已取走快照并停在首个监听器的 barrier 上
    assert!(f1.as_mut().poll(&mut cx).is_pending());
    assert!(f2.as_mut().poll(&mut cx).is_pending());

    let _ = tx1.send(());
    let _ = tx2.send(());
    let (r1, r2) = block_on(futures::future::join(f1, f2));
    assert_eq!(once_calls.load(Ordering::SeqCst), 1);
    assert_eq!([r1, r2], [Some(10), None]);
}

#[test]
fn scoped_withdraw_isolates_unrelated_bindings_and_waits_for_retiring_dependents() {
    // 1. 同一提供者撤回 Bar 时不等待无关的 TaggedCfg 异步消费者（service-dispose.spec.ts:73）
    let root = Context::new();
    let (gate_tx, gate_rx) = oneshot::channel::<()>();
    let rx_slot = Arc::new(Mutex::new(Some(gate_rx)));

    let bar_handle_slot: Arc<Mutex<Option<cordis::ProvideHandle>>> = Arc::new(Mutex::new(None));
    let bhs = Arc::clone(&bar_handle_slot);
    let provider = root.inject([], move |ctx| {
        *bhs.lock().unwrap() = Some(ctx.provide_scoped::<Bar>(Arc::new(Bar(1))));
        ctx.provide::<TaggedCfg>(Arc::new(TaggedCfg::default()));
        Ok(())
    });

    let _fast = root.inject([TypeId::of::<Bar>()], |_| Ok(()));
    let rxs = Arc::clone(&rx_slot);
    let slow = root.inject([TypeId::of::<TaggedCfg>()], move |ctx| {
        let rx = rxs.lock().unwrap().take().unwrap();
        ctx.effect_async(async move {
            let _ = rx.await;
        });
        Ok(())
    });

    let mut disposing_slow: BoxFuture<'_, ()> = Box::pin(slow.dispose());
    let bar_handle = bar_handle_slot.lock().unwrap().take().unwrap();
    let mut withdrawing_bar: BoxFuture<'_, ()> = Box::pin(bar_handle.withdraw());

    let waker = futures::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    assert!(disposing_slow.as_mut().poll(&mut cx).is_pending());
    assert!(
        withdrawing_bar.as_mut().poll(&mut cx).is_ready(),
        "撤回 Bar 不应等待无关服务 TaggedCfg 的慢速卸载"
    );
    let _ = gate_tx.send(());
    block_on(disposing_slow);
    block_on(provider.dispose());

    // 2. 提供者卸载时同时等待「当前活跃绑定」与「此前已先行撤回但尚未排空的绑定」（service-dispose.spec.ts:386）
    let root2 = Context::new();
    let closed = Arc::new(AtomicBool::new(false));
    let tl = Arc::new(Mutex::new(Vec::new()));
    let (tx_cur, rx_cur) = oneshot::channel::<()>();
    let (tx_ret, rx_ret) = oneshot::channel::<()>();
    let rx_cur_slot = Arc::new(Mutex::new(Some(rx_cur)));
    let rx_ret_slot = Arc::new(Mutex::new(Some(rx_ret)));
    let bar_h_slot: Arc<Mutex<Option<cordis::ProvideHandle>>> = Arc::new(Mutex::new(None));

    let cp = Arc::clone(&closed);
    let tp = Arc::clone(&tl);
    let bhs2 = Arc::clone(&bar_h_slot);
    let prov2 = root2.inject([], move |ctx| {
        ctx.provide::<Foo>(Foo::new(1, Arc::clone(&cp), Arc::clone(&tp)));
        *bhs2.lock().unwrap() = Some(ctx.provide_scoped::<Bar>(Arc::new(Bar(1))));
        let cp2 = Arc::clone(&cp);
        let tp2 = Arc::clone(&tp);
        ctx.effect_sync(move || {
            cp2.store(true, Ordering::SeqCst);
            tp2.lock().unwrap().push("provider_closed".into());
        });
        Ok(())
    });

    let rcs = Arc::clone(&rx_cur_slot);
    root2.inject([TypeId::of::<Foo>()], move |ctx| {
        if let Some(rx) = rcs.lock().unwrap().take() {
            let cctx = ctx.clone();
            ctx.effect_async(async move {
                let _ = rx.await;
                cctx.locate::<Foo>().unwrap().read("foo_consumer");
            });
        }
        Ok(())
    });

    let rrs = Arc::clone(&rx_ret_slot);
    let tp_bar = Arc::clone(&tl);
    root2.inject([TypeId::of::<Bar>()], move |ctx| {
        if let Some(rx) = rrs.lock().unwrap().take() {
            let tp_b = Arc::clone(&tp_bar);
            ctx.effect_async(async move {
                let _ = rx.await;
                tp_b.lock().unwrap().push("withdrawn_bar_finished".into());
            });
        }
        Ok(())
    });

    let bh = bar_h_slot.lock().unwrap().take().unwrap();
    let mut withdraw_fut: BoxFuture<'_, ()> = Box::pin(bh.withdraw());
    assert!(withdraw_fut.as_mut().poll(&mut cx).is_pending());

    let mut prov_dispose: BoxFuture<'_, ()> = Box::pin(prov2.dispose());
    assert!(prov_dispose.as_mut().poll(&mut cx).is_pending());
    assert!(!closed.load(Ordering::SeqCst));

    // 先放行当前 Foo 消费者，已撤回的 Bar 消费者仍挂起 → provider 仍不能关闭！
    let _ = tx_cur.send(());
    assert!(prov_dispose.as_mut().poll(&mut cx).is_pending());
    assert!(!closed.load(Ordering::SeqCst));

    let _ = tx_ret.send(());
    block_on(futures::future::join(withdraw_fut, prov_dispose));
    assert!(closed.load(Ordering::SeqCst));
    assert_eq!(
        *tl.lock().unwrap(),
        vec![
            "foo_consumer:open:v1",
            "withdrawn_bar_finished",
            "provider_closed"
        ]
    );
}

#[test]
fn update_hooks_support_prepend_global_and_mid_dispatch_removal() {
    let root = Context::new();
    let global_seen = Arc::new(Mutex::new(Vec::new()));
    let gs = Arc::clone(&global_seen);

    root.on_update_with::<WorkerCfg>(
        EventOptions {
            prepend: false,
            global: true,
        },
        move |cfg, next| {
            gs.lock().unwrap().push(cfg.n);
            next()
        },
    );

    struct HookOrderWorker(Arc<Mutex<Vec<&'static str>>>);
    impl ConfigurableComponent for HookOrderWorker {
        type Config = WorkerCfg;
        fn apply(&self, ctx: &Context, _cfg: WorkerCfg) -> Result<(), ComponentError> {
            let log = Arc::clone(&self.0);
            let off_slot: Arc<Mutex<Option<Disposer>>> = Arc::new(Mutex::new(None));

            let l_a = Arc::clone(&log);
            let os = Arc::clone(&off_slot);
            ctx.on_update::<WorkerCfg>(move |_, next| {
                l_a.lock().unwrap().push("a");
                if let Some(Disposer::Sync(f)) = os.lock().unwrap().take() {
                    f();
                }
                next()
            });

            let l_rem = Arc::clone(&log);
            *off_slot.lock().unwrap() = Some(ctx.on_update::<WorkerCfg>(move |_, next| {
                l_rem.lock().unwrap().push("removed");
                next()
            }));

            let l_b = Arc::clone(&log);
            ctx.on_update_with::<WorkerCfg>(
                EventOptions {
                    prepend: true,
                    global: false,
                },
                move |_, next| {
                    l_b.lock().unwrap().push("prepended");
                    next()
                },
            );

            let l_c = Arc::clone(&log);
            ctx.on_update::<WorkerCfg>(move |_, next| {
                l_c.lock().unwrap().push("c");
                next()
            });
            Ok(())
        }
    }

    let order = Arc::new(Mutex::new(Vec::new()));
    let handle = root.use_with(
        Arc::new(HookOrderWorker(Arc::clone(&order))),
        WorkerCfg {
            ok: true,
            n: 0,
            normalized: false,
        },
    );

    block_on(handle.update(WorkerCfg {
        ok: true,
        n: 1,
        normalized: false,
    }))
    .unwrap();
    block_on(handle.update(WorkerCfg {
        ok: true,
        n: 2,
        normalized: false,
    }))
    .unwrap();

    // 全局钩子跨越子 fiber 重载持续生效；局部钩子遵循 prepend 且跳过分发中移除的钩子
    assert_eq!(*global_seen.lock().unwrap(), vec![1, 2]);
    assert_eq!(
        *order.lock().unwrap(),
        vec!["prepended", "a", "c", "prepended", "a", "c"]
    );
}

#[test]
fn replace_recovers_failed_merges_fibers_and_revalidates_schema() {
    let root = Context::new();

    // 1. replace 恢复 Failed 组件（replace.spec.ts:162）
    let broken: Arc<dyn Component> =
        Arc::new(FnComp(vec![], Box::new(|_| Err("initial failure".into()))));
    let fixed_runs = Arc::new(AtomicUsize::new(0));
    let fr = Arc::clone(&fixed_runs);
    let fixed: Arc<dyn Component> = Arc::new(FnComp(
        vec![],
        Box::new(move |_| {
            fr.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
    ));

    let f_broken = root.use_component(Arc::clone(&broken));
    assert_eq!(f_broken.state(), FiberState::Failed);
    root.replace_component(&broken, Arc::clone(&fixed));
    assert_eq!(f_broken.state(), FiberState::Active);
    assert_eq!(fixed_runs.load(Ordering::SeqCst), 1);

    // 2. 合并进已注册的目标组件时，不重启目标组件原有实例（replace.spec.ts:145）
    let f_existing = root.use_component(Arc::clone(&fixed));
    assert_eq!(fixed_runs.load(Ordering::SeqCst), 2);
    let other: Arc<dyn Component> = Arc::new(FnComp(vec![], Box::new(|_| Ok(()))));
    let f_other = root.use_component(Arc::clone(&other));
    root.replace_component(&other, Arc::clone(&fixed));
    assert_eq!(f_other.state(), FiberState::Active);
    assert_eq!(f_existing.state(), FiberState::Active);
    assert_eq!(
        fixed_runs.load(Ordering::SeqCst),
        3,
        "只运行被迁移的 f_other，不重启已有的 f_existing"
    );
    assert_eq!(root.registry_size(), 1);

    // 3. replace_configurable 使用新 schema 重新校验源配置（replace.spec.ts:46）
    struct SchemaV1;
    impl ConfigurableComponent for SchemaV1 {
        type Config = (u32, u32);
        fn validate(&self, (x, _): Self::Config) -> Result<Self::Config, ComponentError> {
            Ok((x, 1))
        }
        fn apply(&self, _ctx: &Context, _cfg: Self::Config) -> Result<(), ComponentError> {
            Ok(())
        }
    }
    struct SchemaV2(Arc<Mutex<Vec<(u32, u32)>>>);
    impl ConfigurableComponent for SchemaV2 {
        type Config = (u32, u32);
        fn validate(&self, (x, _): Self::Config) -> Result<Self::Config, ComponentError> {
            Ok((x, 2))
        }
        fn apply(&self, _ctx: &Context, cfg: Self::Config) -> Result<(), ComponentError> {
            self.0.lock().unwrap().push(cfg);
            Ok(())
        }
    }

    let seen_v2 = Arc::new(Mutex::new(Vec::new()));
    let v1 = Arc::new(SchemaV1);
    let v2 = Arc::new(SchemaV2(Arc::clone(&seen_v2)));
    let h_cfg = root.use_with(Arc::clone(&v1), (42, 0));
    root.replace_configurable(&v1, v2);
    block_on(h_cfg.await_ready()).unwrap();
    assert_eq!(*seen_v2.lock().unwrap(), vec![(42, 2)]);
}


// ============================================================================
// 17. 无显式 dispose 直接 drop Context 时无引用环泄漏（保证 RAII 锁服务即时释放）
// ============================================================================

#[test]
fn dropping_context_without_dispose_releases_all_services_and_listeners_without_cycles() {
    let svc = Arc::new(Bar(42));
    let comp_token = Arc::new(());
    let listener_token = Arc::new(());

    {
        let ctx = Context::new();
        ctx.provide::<Bar>(Arc::clone(&svc));

        let lt = Arc::clone(&listener_token);
        ctx.on_emit_once_scoped::<EvEmit>(move |_| {
            let _ = &lt;
        });

        let ct = Arc::clone(&comp_token);
        let _handle = ctx.inject([], move |c| {
            let _ = &ct;
            let _ = c.on_update::<u32>(|_, next| next());
            Ok(())
        });

        assert!(Arc::strong_count(&svc) > 1);
        assert!(Arc::strong_count(&comp_token) > 1);
        assert!(Arc::strong_count(&listener_token) > 1);
    }

    assert_eq!(Arc::strong_count(&svc), 1, "provided service must drop with Context");
    assert_eq!(Arc::strong_count(&comp_token), 1, "mounted component must drop with Context");
    assert_eq!(
        Arc::strong_count(&listener_token),
        1,
        "scoped once listener must drop with Context"
    );
}
