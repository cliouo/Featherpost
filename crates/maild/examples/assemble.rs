//! R3 验收样例：maild 守护进程从 TOML 配置装配插件。
//!
//! 运行：`cargo run -p maild --example assemble`
//!
//! 演示：注册表 → `load()` 装配 → 健康报告 → 账户级配置覆盖（intercept_config）→ LIFO 卸载。
//! R4 会把 receive/otp 换成真实领域实现（imap-idle / 验证码提取），装配路径不变。

use std::any::TypeId;
use std::sync::{Arc, Mutex};

use cordis::{Context, Plugin, PluginError, Service};
use maild::config::{intercept_config, Merge};
use maild::loader::{load, plugin, Registry};
use serde::Deserialize;

// —— R4 领域服务的桩实现（R3 只验证装配路径） ——

#[derive(Clone, Debug, Deserialize, Default)]
struct ReceiveConfig {
    #[serde(default)]
    server: String,
    #[serde(default)]
    idle: bool,
}

impl Service for ReceiveConfig {
    fn name(&self) -> &'static str {
        "receive.config"
    }
}

impl Merge for ReceiveConfig {
    fn merge(&self, frag: &Self) -> Self {
        ReceiveConfig {
            server: if frag.server.is_empty() {
                self.server.clone()
            } else {
                frag.server.clone()
            },
            idle: self.idle || frag.idle,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Default)]
struct OtpConfig {
    #[serde(default)]
    pattern: String,
}

impl Service for OtpConfig {
    fn name(&self) -> &'static str {
        "otp.config"
    }
}

impl Merge for OtpConfig {
    fn merge(&self, frag: &Self) -> Self {
        OtpConfig {
            pattern: if frag.pattern.is_empty() {
                self.pattern.clone()
            } else {
                frag.pattern.clone()
            },
        }
    }
}

/// receive 提供的服务（模拟：真实的 imap-idle 客户端）。
struct ImapClient(String);

impl Service for ImapClient {
    fn name(&self) -> &'static str {
        "receive.client"
    }
}

struct ReceivePlugin {
    log: Arc<Mutex<Vec<String>>>,
}

impl Plugin for ReceivePlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<ReceiveConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let cfg = ctx.get::<ReceiveConfig>().expect("inject 就绪");
        self.log
            .lock()
            .unwrap()
            .push(format!("receive: imap-idle → {}", cfg.server));
        // 提供能力给下游插件（真实版：收信事件源）
        ctx.provide::<ImapClient>(Arc::new(ImapClient(cfg.server.clone())));
        Ok(())
    }
}

struct OtpPlugin {
    log: Arc<Mutex<Vec<String>>>,
}

impl Plugin for OtpPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<OtpConfig>(), TypeId::of::<ImapClient>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let cfg = ctx.get::<OtpConfig>().expect("inject 就绪");
        let imap = ctx.get::<ImapClient>().expect("inject 就绪");
        self.log
            .lock()
            .unwrap()
            .push(format!("otp: armed on {} (pattern={:?})", imap.0, cfg.pattern));
        Ok(())
    }
}

const TOML: &str = include_str!("assemble.toml");

fn report(asm: &maild::loader::Assembly, log: &Arc<Mutex<Vec<String>>>) {
    for e in &asm.errors {
        eprintln!("  装配错误: {e}");
    }
    for (name, state) in asm.report() {
        println!("  {name:<10} {state:?}");
    }
    for line in log.lock().unwrap().iter() {
        println!("    | {line}");
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

fn main() {
    // 1. 静态注册表（v1 编译进二进制）
    let mut reg = Registry::default();
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let r_log = Arc::clone(&log);
    reg.register(plugin::<ReceiveConfig, ReceivePlugin, _>(
        "receive",
        move |_, _| Arc::new(ReceivePlugin { log: Arc::clone(&r_log) }),
    ));
    let o_log = Arc::clone(&log);
    reg.register(plugin::<OtpConfig, OtpPlugin, _>("otp", move |_, _| {
        Arc::new(OtpPlugin { log: Arc::clone(&o_log) })
    }));

    let ctx = Context::new();

    // 2. base 装配
    println!("== 1) base 装配（配置见同目录 assemble.toml） ==");
    let mut asm = load(&reg, &ctx, TOML).expect("装配失败");
    report(&asm, &log);

    // 3. 账户级配置覆盖：inner scope 改 server，插件读到合并值
    println!("== 2) 账户级覆盖：server 指向备用入口 ==");
    let scope = intercept_config::<ReceiveConfig>(
        &ctx,
        ReceiveConfig {
            server: "imap.backup.local:993".into(),
            idle: false,
        },
    );
    let mut asm2 = load(&reg, &scope, TOML).expect("装配失败");
    report(&asm2, &log);

    // 4. LIFO 卸载（守护进程退出路径）
    println!("== 3) 卸载 ==");
    block_on(async {
        asm2.dispose().await;
        asm.dispose().await;
    });
    println!("  全部插件已卸载（逆序、无残留）。");
}
