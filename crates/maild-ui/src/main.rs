//! `maild-ui`：默认前端（`maild_brand::UI_BIN`）。端点发现与控制 CLI（`maild_brand::CLI_CTL`）/ 旧桌面壳共用同一套参数与环境变量。

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use maild_client::{discover_endpoint, DiscoveryOptions};

#[derive(Parser)]
#[command(version, about = format!("{} default frontend (Tauri shell)", maild_brand::PRODUCT_NAME))]
struct Cli {
    /// maild TOML config used to discover the local API transport.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Override the loopback HTTP JSON-RPC endpoint (refused: the shell must bind a frontend
    /// principal, which only same-user IPC can carry).
    #[arg(long, value_name = "URL", conflicts_with_all = ["socket", "pipe"])]
    url: Option<String>,
    /// Connect through a Unix domain socket.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["url", "pipe"])]
    socket: Option<PathBuf>,
    /// Connect through a Windows named pipe.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["url", "socket"])]
    pipe: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    // ADR-0044：同一平台 runtime 只有一个主邮件窗口。后到者把已有窗口带到前台后退出；
    // 锁守卫活到 `main` 结束（进程退出即释放）。
    let custom = cli.config.is_some() || cli.url.is_some() || cli.socket.is_some() || cli.pipe.is_some();
    let _instance = match maild_ui::instance_lock::claim(custom) {
        maild_ui::instance_lock::InstanceClaim::Owner(lock) => lock,
        maild_ui::instance_lock::InstanceClaim::Focused => std::process::exit(0),
    };
    let options = DiscoveryOptions {
        config_path: cli.config,
        url: cli.url,
        socket: cli.socket,
        pipe: cli.pipe,
    };
    let endpoint = match discover_endpoint(&options) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("maild-ui: {}", error.message);
            std::process::exit(2);
        }
    };

    // 连接 actor 与 Tauri 共用一个 tokio runtime；runtime 活到进程结束。
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    tauri::async_runtime::set(runtime.handle().clone());
    let bridge = match runtime.block_on(async { maild_ui::Bridge::spawn(endpoint) }) {
        Ok(bridge) => Arc::new(bridge),
        Err(error) => {
            eprintln!("maild-ui: {}", error.message);
            std::process::exit(2);
        }
    };
    if let Err(error) = maild_ui::run(bridge) {
        eprintln!("maild-ui: {error}");
        std::process::exit(1);
    }
}
