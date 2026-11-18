//! Platform-managed, non-interactive daemon host (`maild_brand::AGENT_BIN`).
//!
//! Unlike the interactive daemon CLI (`maild_brand::CLI_DAEMON`), this entrypoint intentionally has no configuration
//! override. It always uses the absolute platform config selected by [`maild_paths::AppPaths`].

use clap::{Parser, Subcommand};
use maild::agent_lifecycle::{AgentLifecycleLock, AgentLifecycleStatus};
use maild::{run_daemon, DaemonOptions};
use maild_paths::AppPaths;
use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

#[derive(Debug, Parser)]
#[command(
    name = maild_brand::AGENT_BIN,
    about = "platform-managed background agent"
)]
struct AgentArgs {
    #[command(subcommand)]
    command: Option<AgentCommand>,
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// Read-only check used by bundle maintenance after the user unregisters the Agent.
    #[command(name = "maintenance-probe")]
    Probe,
    /// Hold the lifecycle lock until the maintenance command closes stdin.
    #[command(name = "maintenance-lease")]
    Lease,
    /// Acquire the lifecycle lock and replace this process with the maintenance manager.
    #[command(name = "maintenance-exec", hide = true)]
    Exec {
        #[arg(long)]
        manager: PathBuf,
        #[arg(last = true)]
        arguments: Vec<OsString>,
    },
    /// Validate the lifecycle descriptor inherited by the exec'd maintenance manager.
    #[command(name = "maintenance-validate-fd", hide = true)]
    ValidateFd {
        #[arg(long)]
        fd: i32,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let arguments = AgentArgs::parse();

    let app_paths = match AppPaths::discover() {
        Ok(paths) => paths,
        Err(_) => {
            eprintln!("[maild-agent] 配置停止: 无法确定当前用户的平台目录");
            return;
        }
    };
    match arguments.command.as_ref() {
        Some(AgentCommand::Probe) => {
            match AgentLifecycleLock::probe(lifecycle_root(&app_paths), &app_paths.runtime_dir) {
                Ok(AgentLifecycleStatus::Idle) => {
                    println!("maintenance-probe: idle");
                    return;
                }
                Ok(AgentLifecycleStatus::Busy) => {
                    eprintln!("[maild-agent] maintenance-probe: busy");
                    std::process::exit(3);
                }
                Err(_) => {
                    eprintln!("[maild-agent] maintenance-probe: 无法安全确认 Agent 已停止");
                    std::process::exit(1);
                }
            }
        }
        Some(AgentCommand::Lease) => {
            let _lease = match AgentLifecycleLock::acquire(
                lifecycle_root(&app_paths),
                &app_paths.runtime_dir,
            ) {
                Ok(lock) => lock,
                Err(_) => {
                    eprintln!("[maild-agent] maintenance-lease: 无法取得 Agent 生命周期锁");
                    std::process::exit(3);
                }
            };
            println!("maintenance-lease: ready");
            if std::io::stdout().flush().is_err() {
                std::process::exit(1);
            }
            let mut buffer = [0_u8; 1024];
            loop {
                match std::io::stdin().read(&mut buffer) {
                    Ok(0) => return,
                    Ok(_) => {}
                    Err(_) => std::process::exit(1),
                }
            }
        }
        Some(AgentCommand::Exec { manager, arguments }) => {
            #[cfg(unix)]
            {
                if !manager.is_absolute() {
                    eprintln!("[maild-agent] maintenance-exec: manager 路径必须是绝对路径");
                    std::process::exit(1);
                }
                let metadata = std::fs::symlink_metadata(manager).unwrap_or_else(|_| {
                    eprintln!("[maild-agent] maintenance-exec: manager 不可用");
                    std::process::exit(1);
                });
                if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                    eprintln!("[maild-agent] maintenance-exec: manager 必须是普通文件");
                    std::process::exit(1);
                }
                let lease =
                    AgentLifecycleLock::acquire(lifecycle_root(&app_paths), &app_paths.runtime_dir)
                        .unwrap_or_else(|_| {
                            eprintln!("[maild-agent] maintenance-exec: 无法取得 Agent 生命周期锁");
                            std::process::exit(3);
                        });
                let descriptor = lease.make_inheritable().unwrap_or_else(|_| {
                    eprintln!("[maild-agent] maintenance-exec: 无法继承 Agent 生命周期锁");
                    std::process::exit(1);
                });
                let validator = std::env::current_exe().unwrap_or_else(|_| {
                    eprintln!("[maild-agent] maintenance-exec: 无法确定 Agent 路径");
                    std::process::exit(1);
                });
                let error = std::process::Command::new(manager)
                    .arg("--internal-maintenance")
                    .args(arguments)
                    .env(maild_brand::MAINTENANCE_LOCK_FD_ENV, descriptor.to_string())
                    .env(maild_brand::MAINTENANCE_VALIDATOR_ENV, validator)
                    .exec();
                drop(lease);
                eprintln!("[maild-agent] maintenance-exec: 无法启动维护事务: {error}");
                std::process::exit(1);
            }
            #[cfg(not(unix))]
            {
                let _ = (manager, arguments);
                eprintln!("[maild-agent] maintenance-exec: 当前平台不支持继承生命周期锁");
                std::process::exit(1);
            }
        }
        Some(AgentCommand::ValidateFd { fd }) => {
            #[cfg(unix)]
            {
                if *fd < 0
                    || AgentLifecycleLock::validate_inherited(
                        lifecycle_root(&app_paths),
                        &app_paths.runtime_dir,
                        *fd,
                    )
                    .is_err()
                {
                    eprintln!("[maild-agent] maintenance-validate-fd: 生命周期锁无效");
                    std::process::exit(1);
                }
                return;
            }
            #[cfg(not(unix))]
            {
                let _ = fd;
                eprintln!("[maild-agent] maintenance-validate-fd: 当前平台不支持继承生命周期锁");
                std::process::exit(1);
            }
        }
        None => {}
    }

    // 只有常驻守护路径需要 GUI 会话，维护子命令都是短命辅助进程（`maintenance-exec` 还会
    // exec 掉自己），不应该顺手建立 WindowServer 连接。
    #[cfg(target_os = "macos")]
    maild::notify::prepare_macos_gui_session();

    // 常驻收信是后台工作（ADR-0041 决策 6）：只在长驻守护路径上降到 UTILITY QoS。
    maild::daemon::apply_background_qos();
    clear_interactive_overrides();
    let _lifecycle =
        match AgentLifecycleLock::acquire(lifecycle_root(&app_paths), &app_paths.runtime_dir) {
            Ok(lock) => lock,
            Err(_) => {
                eprintln!("[maild-agent] 配置停止: 另一个 Agent 正在运行或生命周期锁不安全");
                return;
            }
        };
    let config_path = app_paths.config_file.clone();
    let options = match DaemonOptions::new(config_path, app_paths) {
        Ok(options) => options.with_managed_socket_recovery().with_managed_agent_control(),
        Err(_) => {
            eprintln!("[maild-agent] 配置停止: 平台路径不是有效的绝对路径");
            return;
        }
    };

    // Handled configuration/runtime ownership failures are a clean configuration stop. A Login
    // Item must not turn them into an automatic restart loop. Panics and process crashes retain
    // their normal non-zero status.
    if let Err(error) = run_daemon(options).await {
        // Runner errors may embed TOML parser context. Do not copy those details to a background
        // process log because the source line can contain credentials or bearer tokens.
        if error.is_configuration_stop() {
            eprintln!(
                "[maild-agent] 配置停止: 无法从平台配置启动 {}",
                maild_brand::CLI_DAEMON
            );
        } else {
            eprintln!("[maild-agent] 意外故障: 无法安装后台运行环境");
            std::process::exit(1);
        }
    }
}

fn lifecycle_root(paths: &AppPaths) -> &std::path::Path {
    if paths.runtime_dir.starts_with(&paths.data_dir) {
        &paths.data_dir
    } else {
        &paths.runtime_dir
    }
}

fn clear_interactive_overrides() {
    for name in [
        maild_paths::CONFIG_ENV,
        maild_client::API_TOKEN_ENV,
        maild_client::API_URL_ENV,
        maild_client::API_SOCKET_ENV,
        maild_client::API_PIPE_ENV,
    ] {
        std::env::remove_var(name);
    }
}
