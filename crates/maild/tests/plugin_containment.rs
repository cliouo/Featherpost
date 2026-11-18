//! M4-04 supervised 插件进程 containment 验收（ADR-0020 实施切片）。
//!
//! 全文件 Unix：脚本类 fixture 依赖 sh/进程组语义；Windows Job Object 行为
//! 以真机/CI job 证据为准。
//!
//! 覆盖：父进程正常退出（exit 0）后遗留的组内后台后代被组清扫（孤儿覆盖）；
//! 忽略 SIGTERM 的插件在停止预算内被 SIGKILL 升级且后代一并终止；两种路径终态
//! 均为 `stopped`（清扫成功不算违约残留）。setsid 主动逃逸不可见，是文档化残余
//! 风险（M5 沙箱），不在 hermetic 断言范围内。脚本类用例限 Unix。

#![cfg(unix)]

use std::time::Duration;

use maild::{run_daemon_until, DaemonOptions};
use maild_client::{Endpoint, RpcClient};
use maild_paths::AppPaths;

/// 受管账户目录在平台布局下会装配凭据缝；测试一律走内存 store，绝不碰真 Keychain。
fn use_memory_credential_store() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
}

fn test_paths(root: &std::path::Path) -> AppPaths {
    AppPaths::from_roots(
        &root.join("config"),
        &root.join("data"),
        Some(&root.join("runtime")),
        Some(&root.join("state")),
        root,
    )
}

fn write_config(paths: &AppPaths, extra: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.runtime_dir).unwrap();
    // 受管插件目录由测试预建（生产路径装配要求 ≤0700）。
    std::fs::create_dir_all(&paths.plugin_dir).unwrap();
    std::fs::create_dir_all(&paths.frontend_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut dirs = vec![
            &paths.config_dir,
            &paths.data_dir,
            &paths.runtime_dir,
            &paths.plugin_dir,
            &paths.frontend_dir,
        ];
        dirs.sort();
        dirs.dedup();
        for dir in dirs {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let config = paths.config_file.clone();
    std::fs::write(
        &config,
        format!(
            r#"
            [plugins.store.config]
            db_path = "mail.sqlite3"
            [plugins.api.config]
            transport = "unix"
            listen = "mail.sock"
            [plugins.frontend.config]
            default = "builtin-egui"
            {extra}
            "#
        ),
    )
    .unwrap();
    config
}

fn seal_package(package: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
            let mut out = vec![dir.to_path_buf()];
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.extend(walk(&path));
                } else {
                    out.push(path);
                }
            }
            out
        }
        for entry in walk(package) {
            std::fs::set_permissions(
                &entry,
                std::fs::Permissions::from_mode(if entry.is_dir() { 0o700 } else { 0o600 }),
            )
            .unwrap();
        }
    }
    #[cfg(not(unix))]
    let _ = package;
}

/// 安装一个 supervised fixture：`body` 里可用 `$PKG_DIR`（包绝对路径）。
fn install_supervised_package(paths: &AppPaths, id: &str, body: &str) {
    let package = paths.plugin_dir.join(id);
    std::fs::create_dir_all(package.join("bin")).unwrap();
    std::fs::write(
        package.join("plugin.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Containment fixture\"\nversion = \"1.0.0\"\nlifecycle = \"supervised\"\ncapabilities = [\"messages_read\"]\nexecutable = \"bin/child.sh\"\n"
        ),
    )
    .unwrap();
    let script = package.join("bin/child.sh");
    let body = body.replace("$PKG_DIR", &package.to_string_lossy());
    std::fs::write(&script, body).unwrap();
    seal_package(&package);
    // supervised 可执行文件需要执行位（seal 之后单独放开）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

type DaemonTask = tokio::task::JoinHandle<Result<(), maild::DaemonError>>;

fn spawn_daemon(paths: &AppPaths) -> (DaemonTask, tokio::sync::oneshot::Sender<()>) {
    let options = DaemonOptions::new(paths.config_file.clone(), paths.clone()).expect("绝对路径");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run_daemon_until(options, async move {
        let _ = shutdown_rx.await;
    }));
    (task, shutdown_tx)
}

async fn wait_socket_or_fail(task: &mut DaemonTask, socket: &std::path::Path, secs: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !socket.exists() {
        if tokio::time::Instant::now() >= deadline {
            let error = (&mut *task).await.expect("daemon task").err();
            panic!("socket 未在 {secs}s 内出现；daemon 错误: {error:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn unix_client(socket: &std::path::Path) -> RpcClient {
    RpcClient::new(Endpoint::Unix {
        path: socket.to_path_buf(),
    })
    .expect("受管 socket 连接")
}

async fn wait_until<F: FnMut() -> bool>(mut predicate: F, secs: u64, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !predicate() {
        assert!(tokio::time::Instant::now() < deadline, "超时: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

const CONTAINMENT_ID: &str = "org.example.contained";

async fn plugin_state(user: &RpcClient) -> String {
    user.call("listExternalPlugins", serde_json::json!({}))
        .await
        .unwrap()["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == CONTAINMENT_ID)
        .map(|p| p["state"].as_str().unwrap_or("").to_string())
        .unwrap_or_default()
}

async fn wait_plugin_terminal(user: &RpcClient, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let state = plugin_state(user).await;
        if state == "stopped" || state == "leaked" || state == "failed" {
            assert_eq!(
                state, expected,
                "终态不符（清扫成功应为 stopped 而非 leaked/failed）"
            );
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "插件未在期限内进入终态（最后状态: {state}）"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 心跳脚本：后台 sh 循环每 100ms 向 heart.txt 追加一行（组内违约后代）。
/// `trap_term` 为 true 时忽略 SIGTERM；前台停留 `foreground_secs` 秒后 exit 0。
fn heartbeat_script(trap_term: bool, foreground_secs: u32) -> String {
    let trap_line = if trap_term { "trap '' TERM\n" } else { "" };
    format!(
        "#!/bin/sh\n{trap_line}(while true; do echo x >> \"$PKG_DIR/heart.txt\"; sleep 0.1; done) &\necho started > \"$PKG_DIR/started.txt\"\nsleep {foreground_secs}\nexit 0\n"
    )
}

/// 断言心跳冻结在 `frozen_count`：死掉的后代不再写心跳（存活时应每秒 +10 行）。
async fn assert_heartbeat_frozen(package: &std::path::Path, frozen_count: usize, what: &str) {
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let now = std::fs::read_to_string(package.join("heart.txt"))
        .unwrap_or_default()
        .lines()
        .count();
    assert!(
        now <= frozen_count,
        "{what}: 后代仍存活（心跳从 {frozen_count} 增长到 {now}）"
    );
}

fn heart_lines(package: &std::path::Path) -> usize {
    std::fs::read_to_string(package.join("heart.txt"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn orphans_are_swept_when_parent_exits_voluntarily() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths, "[plugins.external.config]\n");
    // 前台停 3s 再 exit 0：期间心跳累积可观测，退出后清扫必须冻结它。
    install_supervised_package(&paths, CONTAINMENT_ID, &heartbeat_script(false, 3));

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let user = unix_client(&socket);
    user.call(
        "setPluginEnabled",
        serde_json::json!({ "id": CONTAINMENT_ID, "enabled": true }),
    )
    .await
    .unwrap();

    let package = paths.plugin_dir.join(CONTAINMENT_ID);
    wait_until(
        || heart_lines(&package) >= 5,
        10,
        "后代心跳未开始",
    )
    .await;

    // 父进程 exit 0 → supervise 组清扫违约后台后代 → 终态 stopped。
    wait_plugin_terminal(&user, "stopped").await;
    let frozen_count = heart_lines(&package);
    assert_heartbeat_frozen(&package, frozen_count, "父进程正常退出").await;

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn term_ignoring_plugin_is_killed_with_descendants_within_budget() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(
        &paths,
        "[plugins.external.config]\nenabled = [\"org.example.contained\"]\n",
    );
    install_supervised_package(&paths, CONTAINMENT_ID, &heartbeat_script(true, 30));

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let package = paths.plugin_dir.join(CONTAINMENT_ID);
    wait_until(
        || heart_lines(&package) >= 5,
        10,
        "后代心跳未开始",
    )
    .await;

    // disable：SIGTERM 被 trap 忽略（宽限期内心跳继续写是预期行为）→ 预算内
    // SIGKILL 升级；父子与后代一并终止。
    let user = unix_client(&socket);
    user.call(
        "setPluginEnabled",
        serde_json::json!({ "id": CONTAINMENT_ID, "enabled": false }),
    )
    .await
    .unwrap();
    wait_plugin_terminal(&user, "stopped").await;
    // 终态之后才允许冻结：宽限期内的增长不算违约存活。
    let frozen_count = heart_lines(&package);
    assert_heartbeat_frozen(&package, frozen_count, "SIGKILL 升级").await;
    let listing = user.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    let enabled = listing["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == CONTAINMENT_ID)
        .map(|p| p["enabled"].as_bool().unwrap_or(true));
    assert_eq!(enabled, Some(false));

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}
