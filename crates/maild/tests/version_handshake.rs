//! M3.7-04 内核版本握手与重启恢复验收（ADR-0016 实施切片）。
//!
//! 验证协议契约：`getAgentInfo` 身份字段、判定表在真实 daemon 上的行为、
//! 以及一键重启依赖的两条生命周期路径——优雅停止后 socket 消失、硬停后
//! 残留 socket 由受管回收清理。信号发送本身由平台 SIGTERM 测试单独钉死；
//! 本文件用注入 shutdown / 任务 abort 分别模拟两者（daemon 进程内运行，
//! 真实信号会终止测试进程）。

use std::time::Duration;

use maild::{run_daemon_until, DaemonOptions};
use maild_client::{CompatVerdict, Endpoint, RpcClient};
use maild_paths::AppPaths;

fn test_paths(root: &std::path::Path) -> AppPaths {
    AppPaths::from_roots(
        &root.join("config"),
        &root.join("data"),
        Some(&root.join("runtime")),
        Some(&root.join("state")),
        root,
    )
}

fn write_platform_config(paths: &AppPaths) -> std::path::PathBuf {
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.runtime_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for dir in [&paths.config_dir, &paths.data_dir, &paths.runtime_dir] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let config = paths.config_file.clone();
    std::fs::write(
        &config,
        r#"
            [plugins.store.config]
            db_path = "mail.sqlite3"
            [plugins.api.config]
            transport = "unix"
            listen = "mail.sock"
        "#,
    )
    .unwrap();
    config
}

type DaemonTask = tokio::task::JoinHandle<Result<(), maild::DaemonError>>;

/// 启动进程内 daemon；返回注入式 shutdown（与 SIGTERM 共享 LIFO teardown 路径）。
fn spawn_daemon(
    paths: &AppPaths,
    managed_recovery: bool,
) -> (
    DaemonTask,
    tokio::sync::oneshot::Sender<()>,
    std::path::PathBuf,
) {
    let mut options =
        DaemonOptions::new(write_platform_config(paths), paths.clone()).expect("绝对路径");
    if managed_recovery {
        options = options.with_managed_socket_recovery();
    }
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run_daemon_until(options, async move {
        let _ = shutdown_rx.await;
    }));
    let socket = paths.runtime_dir.join("mail.sock");
    (task, shutdown_tx, socket)
}

async fn wait_socket(socket: &std::path::Path, appear: bool, secs: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while socket.exists() != appear {
        assert!(
            tokio::time::Instant::now() < deadline,
            "socket 未在 {secs}s 内{}",
            if appear { "出现" } else { "消失" }
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 轮询直到握手给出期望结论（新内核启动有延迟）。
#[cfg(unix)]
async fn wait_verdict(socket: &std::path::Path, expected: CompatVerdict) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(client) = RpcClient::new(Endpoint::Unix {
            path: socket.to_path_buf(),
        }) {
            if matches!(client.agent_compat(maild_client::PROTOCOL_VERSION).await, Ok(v) if v == expected)
            {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "内核未在期限内给出 {expected:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn handshake_reports_identity_and_generation_verdicts() {
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    let (task, shutdown, socket) = spawn_daemon(&paths, false);
    wait_socket(&socket, true, 10).await;

    let client = RpcClient::new(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    let info = client.agent_info().await.unwrap();
    // 进程内 daemon：pid 即测试进程；诊断字段非空但不参与判定。
    assert_eq!(info.pid, std::process::id());
    assert!(info.started_at.unwrap_or(0) > 0);
    assert!(!info.executable_path.unwrap_or_default().is_empty());

    assert_eq!(
        client
            .agent_compat(maild_client::PROTOCOL_VERSION)
            .await
            .unwrap(),
        CompatVerdict::Compatible
    );
    assert_eq!(
        client
            .agent_compat(maild_client::PROTOCOL_VERSION + 1)
            .await
            .unwrap(),
        CompatVerdict::KernelOutdated
    );

    drop(client);
    let _ = shutdown.send(());
    let _ = task.await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn graceful_stop_removes_socket_and_next_kernel_rehandshakes() {
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    let (task, shutdown, socket) = spawn_daemon(&paths, false);
    wait_socket(&socket, true, 10).await;

    // 一键重启的第一段：优雅停止（SIGTERM 同路径）。teardown 删除受管 socket。
    shutdown.send(()).unwrap();
    task.await.unwrap().unwrap();
    wait_socket(&socket, false, 10).await;

    // 第二段：拉起新内核并重握手。
    let (_restarted, _shutdown, _) = spawn_daemon(&paths, false);
    wait_verdict(&socket, CompatVerdict::Compatible).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn hard_stop_leftover_socket_is_reclaimed_by_managed_restart() {
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    let (task, _shutdown, socket) = spawn_daemon(&paths, false);
    wait_socket(&socket, true, 10).await;

    // 模拟 SIGKILL：任务被硬取消，跳过 teardown，socket 文件残留。
    task.abort();
    let _ = task.await;
    assert!(socket.exists(), "硬停后 socket 应残留等待回收");

    // 新内核以受管模式启动：回收残留 socket 并完成握手。
    let (_restarted, _shutdown, _) = spawn_daemon(&paths, true);
    wait_verdict(&socket, CompatVerdict::Compatible).await;
}
