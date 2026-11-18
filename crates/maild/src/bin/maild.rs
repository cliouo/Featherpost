//! 守护进程与维护命令入口（二进制名为 `maild_brand::CLI_DAEMON`）。
//!
//! 用法：`<CLI_DAEMON> [config.toml]` 启动守护进程；`<CLI_DAEMON> backup ...` 创建在线数据库
//! 备份；`<CLI_DAEMON> restore ...` 在守护进程停止时执行可回滚恢复。
//! 默认配置由 maild-paths 选择；样例见 crate 根 `maild.example.toml`（按品牌写作 featherpost.toml）。

use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;
use maild::store::{backup_database, restore_database};
use maild::{run_daemon, DaemonOptions};
use maild_paths::AppPaths;

#[derive(Debug, Parser)]
#[command(
    name = "backup",
    bin_name = format!("{} backup", maild_brand::CLI_DAEMON),
    about = format!("创建经过校验的 {} SQLite 在线备份", maild_brand::PRODUCT_NAME)
)]
struct BackupArgs {
    /// 现有 SQLite 数据库文件
    #[arg(long, value_name = "SOURCE.sqlite3")]
    database: PathBuf,

    /// 必须尚不存在的备份文件路径
    #[arg(long, value_name = "DEST.sqlite3")]
    output: PathBuf,
}

#[derive(Debug, Parser)]
#[command(
    name = "restore",
    bin_name = format!("{} restore", maild_brand::CLI_DAEMON),
    about = format!("在维护锁保护下恢复经过校验的 {} SQLite 备份", maild_brand::PRODUCT_NAME)
)]
struct RestoreArgs {
    /// 必须停止使用的现有 SQLite 数据库
    #[arg(long, value_name = "CURRENT.sqlite3")]
    database: PathBuf,

    /// 经过校验后将替换当前数据库的备份文件
    #[arg(long, value_name = "BACKUP.sqlite3")]
    input: PathBuf,

    /// 必须尚不存在；恢复前保存当前数据库的完整回滚快照
    #[arg(long, value_name = "ROLLBACK.sqlite3")]
    rollback: PathBuf,
}

#[derive(Debug, Parser)]
#[command(
    name = maild_brand::CLI_DAEMON,
    about = format!("{} headless mail daemon", maild_brand::PRODUCT_NAME)
)]
struct DaemonArgs {
    /// Explicit TOML configuration path.
    #[arg(long, value_name = "PATH", conflicts_with = "legacy_config")]
    config: Option<PathBuf>,

    /// Backward-compatible positional TOML configuration path.
    #[arg(value_name = "CONFIG", conflicts_with = "config")]
    legacy_config: Option<PathBuf>,
}

#[derive(Debug, Parser)]
#[command(
    name = "paths",
    bin_name = format!("{} paths", maild_brand::CLI_DAEMON),
    about = format!("显示当前用户的 {} 平台目录", maild_brand::PRODUCT_NAME)
)]
struct PathsArgs {}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args_os();
    let program = args.next().unwrap_or_else(|| OsString::from(maild_brand::CLI_DAEMON));
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("backup")) {
        let backup_args = BackupArgs::try_parse_from(std::iter::once(program).chain(args))
            .unwrap_or_else(|error| error.exit());
        backup_database(&backup_args.database, &backup_args.output).unwrap_or_else(|error| {
            eprintln!("备份失败: {}", terminal_error(&error));
            std::process::exit(1);
        });
        println!("备份完成: {:?}", backup_args.output);
        return;
    }
    if first.as_deref() == Some(std::ffi::OsStr::new("restore")) {
        let restore_args = RestoreArgs::try_parse_from(std::iter::once(program).chain(args))
            .unwrap_or_else(|error| error.exit());
        let outcome = restore_database(
            &restore_args.database,
            &restore_args.input,
            &restore_args.rollback,
        )
        .unwrap_or_else(|error| {
            eprintln!("恢复失败: {}", terminal_error(&error));
            std::process::exit(1);
        });
        println!(
            "恢复完成: {:?}；回滚快照: {:?}",
            outcome.database, outcome.rollback
        );
        return;
    }
    if first.as_deref() == Some(std::ffi::OsStr::new("paths")) {
        PathsArgs::try_parse_from(std::iter::once(program).chain(args))
            .unwrap_or_else(|error| error.exit());
        let paths = AppPaths::discover().unwrap_or_else(|error| {
            eprintln!("路径发现失败: {}", terminal_error(&error));
            std::process::exit(1);
        });
        print_paths(&paths);
        return;
    }

    let daemon_args = DaemonArgs::try_parse_from(std::iter::once(program).chain(first).chain(args))
        .unwrap_or_else(|error| error.exit());
    let explicit = daemon_args.config.or(daemon_args.legacy_config);
    let resolved_config =
        maild_paths::resolve_config(explicit.as_deref()).unwrap_or_else(|error| {
            eprintln!("配置路径错误: {}", terminal_error(&error));
            std::process::exit(1);
        });
    let app_paths = AppPaths::discover().unwrap_or_else(|error| {
        eprintln!("路径发现失败: {}", terminal_error(&error));
        std::process::exit(1);
    });
    let options = DaemonOptions::new(resolved_config.path, app_paths)
        .unwrap_or_else(|error| {
            eprintln!("配置路径错误: {}", terminal_error(&error));
            std::process::exit(1);
        })
        .with_legacy_cwd(std::env::current_dir().ok());

    if let Err(error) = run_daemon(options).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn print_paths(paths: &AppPaths) {
    print_path("config", &paths.config_file);
    print_path("data", &paths.data_dir);
    print_path("database", &paths.database_file);
    print_path("runtime", &paths.runtime_dir);
    print_path("socket", &paths.socket_file);
    print_path("backups", &paths.backup_dir);
    print_path("releases", &paths.release_dir);
    print_path("frontends", &paths.frontend_dir);
    print_path("plugins", &paths.plugin_dir);
    print_path("plugin-data", &paths.plugin_data_dir);
    print_path("logs", &paths.log_dir);
}

fn print_path(name: &str, path: &std::path::Path) {
    println!("{name}={}", terminal_error(&path.display()));
}

fn terminal_error(error: &dyn std::fmt::Display) -> String {
    const MAX_CHARS: usize = 2_048;

    let mut output = String::with_capacity(MAX_CHARS);
    let mut truncated = false;
    for (count, character) in error.to_string().chars().enumerate() {
        if count == MAX_CHARS {
            truncated = true;
            break;
        }
        output.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    if truncated {
        output.push('…');
    }
    output
}
