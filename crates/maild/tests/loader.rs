//! R3 loader 一致性测试：TOML 声明式配置 → 插件装配。
//!
//! 覆盖：装配 / enabled 跳过 / 未知插件与坏配置的非致命错误 / TOML 语法错误 /
//! 配置覆盖（intercept_config 内层赢）/ LIFO 卸载。

use std::any::TypeId;
use std::sync::{Arc, Mutex};

use cordis::{Context, Plugin, PluginError, Service};
use maild::config::{intercept_config, Merge};
use maild::loader::{load, plugin, plugin_config, LoadError, Registry};
use maild::scheduler::SchedulerConfig;
use serde::Deserialize;

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

// —— 演示用领域类型 ——

#[derive(Clone, Debug, PartialEq, Deserialize, Default)]
struct GreeterConfig {
    #[serde(default)]
    message: String,
    #[serde(default)]
    loud: bool,
}

impl Service for GreeterConfig {
    fn name(&self) -> &'static str {
        "greeter.config"
    }
}

impl Merge for GreeterConfig {
    /// 字段级覆盖：非空 message 覆盖，loud 取或。
    fn merge(&self, frag: &Self) -> Self {
        GreeterConfig {
            message: if frag.message.is_empty() {
                self.message.clone()
            } else {
                frag.message.clone()
            },
            loud: self.loud || frag.loud,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Default)]
struct ClockConfig {
    #[serde(default = "default_tz")]
    tz: String,
}

fn default_tz() -> String {
    "UTC".into()
}

impl Service for ClockConfig {
    fn name(&self) -> &'static str {
        "clock.config"
    }
}

impl Merge for ClockConfig {
    fn merge(&self, frag: &Self) -> Self {
        ClockConfig {
            tz: if frag.tz.is_empty() {
                self.tz.clone()
            } else {
                frag.tz.clone()
            },
        }
    }
}

/// 记录 apply / 卸载时刻的日志。
#[derive(Default)]
struct Log {
    events: Mutex<Vec<String>>,
}

impl Log {
    fn record(&self, s: impl Into<String>) {
        self.events.lock().unwrap().push(s.into());
    }
    fn all(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

/// 演示插件：inject 配置，apply 里 `get::<GreeterConfig>()`（读到 intercept 合并值），
/// 注册同步逆（卸载时记录）。
struct Greeter {
    log: Arc<Log>,
}

impl Plugin for Greeter {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<GreeterConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let cfg = ctx.get::<GreeterConfig>().expect("inject 就绪");
        self.log.record(format!(
            "greeter:{}:{}",
            cfg.message,
            if cfg.loud { "!" } else { "." }
        ));
        let log = Arc::clone(&self.log);
        ctx.effect_sync(move || log.record("greeter:disposed"));
        Ok(())
    }
}

/// 消费其他插件配置的服务（演示跨插件 inject）。
struct Banner {
    log: Arc<Log>,
}

impl Plugin for Banner {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<GreeterConfig>(), TypeId::of::<ClockConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let g = ctx.get::<GreeterConfig>().expect("inject 就绪");
        let c = ctx.get::<ClockConfig>().expect("inject 就绪");
        self.log
            .record(format!("banner:[{} @ {}]", g.message, c.tz));
        Ok(())
    }
}

fn registry() -> (Registry, Arc<Log>) {
    let mut reg = Registry::default();
    let log = Arc::new(Log::default());

    let g_log = Arc::clone(&log);
    reg.register(plugin::<GreeterConfig, Greeter, _>(
        "greeter",
        move |_, _| {
            Arc::new(Greeter {
                log: Arc::clone(&g_log),
            })
        },
    ));

    let c_log = Arc::clone(&log);
    reg.register(plugin::<ClockConfig, Banner, _>("clock", move |_, _| {
        Arc::new(Banner {
            log: Arc::clone(&c_log),
        })
    }));

    (reg, log)
}

// —— 装配 ——

#[test]
fn loads_plugins_from_toml_in_file_order() {
    let (reg, log) = registry();
    let ctx = Context::new();

    let mut asm = load(
        &reg,
        &ctx,
        r#"
[plugins.greeter]
[plugins.greeter.config]
message = "你好"
loud = true

[plugins.clock]
[plugins.clock.config]
tz = "Asia/Shanghai"
"#,
    )
    .expect("装配成功");

    assert!(asm.errors.is_empty(), "无错误: {:?}", asm.errors);
    assert_eq!(
        log.all(),
        vec![
            "greeter:你好:!".to_string(),
            "banner:[你好 @ Asia/Shanghai]".to_string(),
        ],
        "按配置书写顺序装配，跨插件 inject 解析到配置"
    );

    // LIFO 卸载
    block_on(async { asm.dispose().await });
    assert_eq!(
        log.all(),
        vec![
            "greeter:你好:!".to_string(),
            "banner:[你好 @ Asia/Shanghai]".to_string(),
            "greeter:disposed".to_string(),
        ],
        "卸载执行逆"
    );
}

#[test]
fn missing_config_falls_back_to_default() {
    let (reg, log) = registry();
    let ctx = Context::new();

    let asm = load(&reg, &ctx, "[plugins.greeter]\n").expect("装配成功");
    assert!(asm.errors.is_empty());
    assert_eq!(log.all(), vec!["greeter::.".to_string()], "默认配置");
}

#[test]
fn disabled_plugin_is_skipped() {
    let (reg, log) = registry();
    let ctx = Context::new();

    let asm = load(
        &reg,
        &ctx,
        r#"
[plugins.greeter]
enabled = false
[plugins.clock]
"#,
    )
    .expect("装配成功");

    assert!(asm.errors.is_empty());
    assert_eq!(
        log.all(),
        Vec::<String>::new(),
        "greeter 被跳过 → 无人提供 GreeterConfig"
    );
    // banner 依赖 GreeterConfig：保持 Idle 等待（响应式 inject 语义）
    assert_eq!(
        asm.report(),
        vec![("clock".to_string(), cordis::FiberState::Idle)]
    );
}

#[test]
fn unknown_plugin_is_reported_but_does_not_block() {
    let (reg, log) = registry();
    let ctx = Context::new();

    let asm = load(
        &reg,
        &ctx,
        r#"
[plugins.mystery]
[plugins.greeter]
"#,
    )
    .expect("装配成功");

    assert_eq!(asm.errors.len(), 1);
    assert!(matches!(asm.errors[0], LoadError::UnknownPlugin(ref n) if n == "mystery"));
    assert_eq!(
        log.all(),
        vec!["greeter::.".to_string()],
        "其余插件照常装配"
    );
}

#[test]
fn bad_config_is_reported_and_skipped() {
    let (reg, log) = registry();
    let ctx = Context::new();

    let asm = load(
        &reg,
        &ctx,
        r#"
[plugins.greeter]
[plugins.greeter.config]
message = 42
[plugins.clock]
"#,
    )
    .expect("装配成功");

    assert_eq!(asm.errors.len(), 1);
    assert!(matches!(asm.errors[0], LoadError::Config { ref plugin, .. } if plugin == "greeter"));
    assert_eq!(
        log.all(),
        Vec::<String>::new(),
        "坏配置项被跳过 → GreeterConfig 未提供"
    );
    assert_eq!(
        asm.report(),
        vec![("clock".to_string(), cordis::FiberState::Idle)]
    );
}

#[test]
fn parse_error_is_fatal() {
    let (reg, _) = registry();
    let ctx = Context::new();
    assert!(matches!(
        load(&reg, &ctx, "[plugins.greeter\n"),
        Err(LoadError::Parse(_))
    ));
}

#[test]
fn non_table_plugins_is_fatal() {
    let (reg, _) = registry();
    let ctx = Context::new();
    assert!(matches!(
        load(&reg, &ctx, "plugins = 1\n"),
        Err(LoadError::Structure(_))
    ));
}

#[test]
fn plugin_config_uses_loader_structure_and_enabled_semantics() {
    let cfg = plugin_config::<GreeterConfig>(
        r#"
[plugins.greeter]
[plugins.greeter.config]
message = "hello"
"#,
        "greeter",
    )
    .unwrap()
    .expect("启用插件配置");
    assert_eq!(cfg.message, "hello");

    assert!(
        plugin_config::<GreeterConfig>("[plugins.greeter]\nenabled = false\n", "greeter")
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        plugin_config::<GreeterConfig>("plugins = 1\n", "greeter"),
        Err(LoadError::Structure(_))
    ));
}

// —— 配置覆盖（intercept）与装配的组合 ——

#[test]
fn intercept_config_overrides_inner_scope() {
    let (reg, log) = registry();
    let ctx = Context::new();

    // 先按 base 装配 root
    let mut asm_root = load(&reg, &ctx, "[plugins.greeter]\n").expect("装配成功");
    assert_eq!(log.all(), vec!["greeter::.".to_string()]);

    // 带覆盖的子 scope：message 覆盖为「你好」——插件挂在该 scope 上读到合并值
    let scope = intercept_config::<GreeterConfig>(
        &ctx,
        GreeterConfig {
            message: "你好".into(),
            loud: false,
        },
    );
    let mut asm_scope = load(&reg, &scope, "[plugins.greeter]\n").expect("装配成功");
    assert_eq!(
        log.all(),
        vec!["greeter::.".to_string(), "greeter:你好:.".to_string()],
        "内层 scope 读到覆盖后的配置；root 不受影响"
    );

    // 双读稳定性：scope 里再次 get，仍是覆盖值
    assert_eq!(
        scope.get::<GreeterConfig>().map(|c| c.message.clone()),
        Some("你好".into())
    );
    assert_eq!(
        ctx.get::<GreeterConfig>().map(|c| c.message.clone()),
        Some("".into()),
        "root 绑定未被污染"
    );

    block_on(async {
        asm_scope.dispose().await;
        asm_root.dispose().await;
    });
}

#[test]
fn global_registry_exposes_scheduler_config() {
    let names = maild::global_registry().names();
    assert!(names.contains(&"scheduler"));
    assert!(names.contains(&"frontend"));
    assert!(names.contains(&"external"));
    assert!(names.contains(&"menu"));

    let cfg = plugin_config::<SchedulerConfig>("[plugins.scheduler]\n", "scheduler")
        .unwrap()
        .expect("scheduler enabled");
    assert_eq!(cfg.jitter_secs, 1);

    assert!(plugin_config::<SchedulerConfig>(
        "[plugins.scheduler]\nenabled = false\n",
        "scheduler",
    )
    .unwrap()
    .is_none());
}
