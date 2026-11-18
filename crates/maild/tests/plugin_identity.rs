//! M4-01 插件身份与认证缝验收（ADR-0017 实施切片）。
//!
//! 覆盖：未认证会话恒为 user、reveal 即轮换、authenticate 绑定与 -32014 断连、
//! HTTP 来源 -32015、包移除后重启吊销、注册表损坏时身份缝软停摆（supervised
//! 拒绝 spawn 且核心照常）、每 spawn 凭证轮换注入。脚本依赖的用例限 Unix；
//! Windows named pipe 认证路径由 CI job 钉死。

use std::time::Duration;

use maild::{run_daemon_until, DaemonOptions};
use maild_client::{ClientError, Endpoint, RpcClient};
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

/// 平台布局配置；`extra` 追加插件节（store/api 由调用方按需覆盖）。
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

const AUTH_PLUGIN_ID: &str = "org.example.auth";

/// 在受管 plugins 目录写入一个 independent fixture 包。
fn install_external_package(paths: &AppPaths, id: &str) {
    let package = paths.plugin_dir.join(id);
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("plugin.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Auth fixture\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"messages_read\"]\n"
        ),
    )
    .unwrap();
    seal_package(&package);
}

#[cfg(unix)]
fn install_supervised_package(paths: &AppPaths, id: &str, body: &str) {
    let package = paths.plugin_dir.join(id);
    std::fs::create_dir_all(package.join("bin")).unwrap();
    std::fs::write(
        package.join("plugin.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Spawn fixture\"\nversion = \"1.0.0\"\nlifecycle = \"supervised\"\ncapabilities = [\"messages_read\"]\nexecutable = \"bin/child.sh\"\n"
        ),
    )
    .unwrap();
    let script = package.join("bin/child.sh");
    std::fs::write(&script, body).unwrap();
    seal_package(&package);
}

fn seal_package(package: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for entry in walk(package) {
            std::fs::set_permissions(
                &entry,
                std::fs::Permissions::from_mode(if entry.is_dir() { 0o700 } else { 0o600 }),
            )
            .unwrap();
        }
        // 可执行位单独恢复给脚本。
        let script = package.join("bin/child.sh");
        if script.exists() {
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    #[cfg(not(unix))]
    let _ = package;
}

#[cfg(unix)]
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

type DaemonTask = tokio::task::JoinHandle<Result<(), maild::DaemonError>>;

fn spawn_daemon(paths: &AppPaths) -> (DaemonTask, tokio::sync::oneshot::Sender<()>) {
    let options =
        DaemonOptions::new(paths.config_file.clone(), paths.clone()).expect("绝对路径");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run_daemon_until(options, async move {
        let _ = shutdown_rx.await;
    }));
    (task, shutdown_tx)
}

/// 等 socket；超时则取回 daemon 错误一起 panic（启动即失败的根因不会丢；
/// 走到超时分支即 daemon 已退出，无需再注入 shutdown）。
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

/// 等待 socket 消失（优雅停止后）。
async fn wait_socket_gone(socket: &std::path::Path, secs: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while socket.exists() {
        assert!(tokio::time::Instant::now() < deadline, "socket 未在 {secs}s 内消失");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn unix_client(socket: &std::path::Path) -> RpcClient {
    RpcClient::new(Endpoint::Unix {
        path: socket.to_path_buf(),
    })
    .expect("受管 socket 连接")
}

fn auth_error_code(error: &ClientError) -> Option<i64> {
    error.rpc_code
}

#[tokio::test(flavor = "current_thread")]
async fn unauthenticated_user_reveal_rotation_and_plugin_binding() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths, "[plugins.external.config]\n");
    install_external_package(&paths, AUTH_PLUGIN_ID);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    // 未认证会话恒为 user。
    let client = unix_client(&socket);
    assert_eq!(client.whoami().await.unwrap(), "user");

    // 扫描对账已建立条目；builtin 前端不参与逐插件身份。
    let frontends = client.call("listFrontends", serde_json::json!({})).await.unwrap();
    assert_eq!(frontends["frontends"][0]["has_identity"], false);
    let listing = client.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    assert_eq!(
        listing["plugins"][0]["has_identity"], true,
        "扫描后应有身份条目"
    );
    let token_one = client.reveal_plugin_token(AUTH_PLUGIN_ID, None).await.unwrap();
    assert!(token_one.starts_with("mdp_") && token_one.len() == 47);

    // 认证绑定主体（连接级长连接），且连接内不可变。
    let mut plugin_conn = client.connect_stream().await.unwrap();
    plugin_conn.authenticate(&token_one).await.unwrap();
    let principal = plugin_conn.call("whoami", serde_json::json!({}), |_| {}).await.unwrap();
    assert_eq!(principal["principal"], format!("plugin:{AUTH_PLUGIN_ID}"));
    // 已认证会话重复 authenticate → -32014，应答后连接关闭。
    let error = plugin_conn.authenticate(&token_one).await.unwrap_err();
    assert_eq!(auth_error_code(&error), Some(-32014));
    assert!(
        plugin_conn.call("whoami", serde_json::json!({}), |_| {}).await.is_err(),
        "-32014 应答后连接必须关闭"
    );

    // 再 reveal 轮换：旧凭证对新连接失效。
    let token_two = client.reveal_plugin_token(AUTH_PLUGIN_ID, None).await.unwrap();
    assert_ne!(token_one, token_two);
    let mut stale = client.connect_stream().await.unwrap();
    let error = stale.authenticate(&token_one).await.unwrap_err();
    assert_eq!(auth_error_code(&error), Some(-32014));
    assert!(
        stale.call("whoami", serde_json::json!({}), |_| {}).await.is_err(),
        "无效凭证同样断连"
    );
    let mut fresh_conn = client.connect_stream().await.unwrap();
    fresh_conn.authenticate(&token_two).await.unwrap();
    let principal = fresh_conn.call("whoami", serde_json::json!({}), |_| {}).await.unwrap();
    assert_eq!(principal["principal"], format!("plugin:{AUTH_PLUGIN_ID}"));

    // 注册表落在活动配置同目录且只含哈希形态条目。
    let registry = std::fs::read_to_string(paths.config_dir.join("plugin_auth.toml")).unwrap();
    assert!(registry.contains("org.example.auth"));
    assert!(!registry.contains(&token_two), "明文绝不落盘");

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn http_origin_rejects_authenticate_but_stays_usable() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    // 抓一个空闲回环端口。
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let bearer = "bearer-token-0123456789abcdef0123456789abcdef";
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.runtime_dir).unwrap();
    // 受管插件目录由测试预建（生产路径装配要求 ≤0700）。
    std::fs::create_dir_all(&paths.plugin_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for dir in [&paths.config_dir, &paths.data_dir, &paths.runtime_dir, &paths.plugin_dir] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    std::fs::write(
        &paths.config_file,
        format!(
            r#"
            [plugins.store.config]
            db_path = "mail.sqlite3"
            [plugins.api.config]
            transport = "http"
            listen = "127.0.0.1:{port}"
            bearer_token = "{bearer}"
            [plugins.external.config]
            "#
        ),
    )
    .unwrap();
    // 含 bearer 的配置文件必须 0600（validate_config_file_security 门禁）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            &paths.config_file,
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    install_external_package(&paths, AUTH_PLUGIN_ID);
    let (_task, _shutdown) = spawn_daemon(&paths);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let client = loop {
        if let Ok(client) = RpcClient::new(Endpoint::Http {
            url: format!("http://127.0.0.1:{port}/rpc"),
            token: bearer.into(),
        }) {
            if client.whoami().await.is_ok() {
                break client;
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "HTTP 未在期限内就绪");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // HTTP 来源恒为 user：authenticate 返回 -32015 但连接继续可用。
    assert_eq!(client.whoami().await.unwrap(), "user");
    let error = client
        .call(
            "authenticate",
            serde_json::json!({ "token": format!("mdp_{}", "A".repeat(43)) }),
        )
        .await
        .unwrap_err();
    assert_eq!(auth_error_code(&error), Some(-32015));
    assert_eq!(client.whoami().await.unwrap(), "user");
    // 凭证发放同样拒绝 HTTP（INVALID_REQUEST 来源门禁）。
    let error = client
        .reveal_plugin_token(AUTH_PLUGIN_ID, None)
        .await
        .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32600), "来源门禁走 INVALID_REQUEST");
}

#[tokio::test(flavor = "current_thread")]
async fn removed_package_revokes_token_after_restart() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths, "[plugins.external.config]\n");
    install_external_package(&paths, AUTH_PLUGIN_ID);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;
    let client = unix_client(&socket);
    let token = client.reveal_plugin_token(AUTH_PLUGIN_ID, None).await.unwrap();

    // 优雅停止 → 移除包 → 重启。
    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
    wait_socket_gone(&socket, 10).await;
    std::fs::remove_dir_all(paths.plugin_dir.join(AUTH_PLUGIN_ID)).unwrap();
    let (mut task, shutdown) = spawn_daemon(&paths);
    wait_socket_or_fail(&mut task, &socket, 10).await;

    // 孤儿条目被收敛；旧凭证对新连接拒绝。
    let client = unix_client(&socket);
    let mut stale_conn = client.connect_stream().await.unwrap();
    let error = stale_conn.authenticate(&token).await.unwrap_err();
    assert_eq!(auth_error_code(&error), Some(-32014));
    assert!(
        stale_conn.call("whoami", serde_json::json!({}), |_| {}).await.is_err()
    );
    let survivor = unix_client(&socket);
    let listing = survivor.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    assert_eq!(
        listing["plugins"].as_array().unwrap().len(),
        0,
        "包移除后清单不再包含该插件"
    );
    let registry = std::fs::read_to_string(paths.config_dir.join("plugin_auth.toml")).unwrap();
    assert!(!registry.contains(AUTH_PLUGIN_ID), "孤儿条目应被收敛");

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

#[cfg(unix)]
const SPAWN_ID: &str = "org.example.spawn";
#[cfg(unix)]
const SPAWN_MARKER: &str = "seen.txt";

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn corrupt_registry_blocks_spawn_and_authenticate_but_core_survives() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    install_supervised_package(
        &paths,
        SPAWN_ID,
        &format!("#!/bin/sh\nprintf x >> {SPAWN_MARKER}\nsleep 30\n"),
    );
    write_config(
        &paths,
        &format!(
            "[plugins.external.config]\nenabled = [\"{SPAWN_ID}\"]\nrestart_initial_secs = 1\n"
        ),
    );
    // 启动前把注册表写坏：身份缝整体停摆。
    std::fs::write(paths.config_dir.join("plugin_auth.toml"), "format_version = 99\n").unwrap();

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    // 核心照常服务；supervised 拒绝 spawn（标记文件不出现）。
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !paths.plugin_dir.join(SPAWN_ID).join(SPAWN_MARKER).exists(),
        "身份缝停摆时 supervised 插件不得启动"
    );
    let client = unix_client(&socket);
    let listing = client.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    assert_eq!(listing["plugins"][0]["state"], "blocked");
    assert_eq!(listing["plugins"][0]["has_identity"], false);
    // authenticate 与 reveal 都返回 -32015（-32015 不关闭连接）。
    let mut auth_conn = client.connect_stream().await.unwrap();
    let error = auth_conn
        .authenticate(&format!("mdp_{}", "A".repeat(43)))
        .await
        .unwrap_err();
    assert_eq!(auth_error_code(&error), Some(-32015));
    let survivor = unix_client(&socket);
    let error = survivor
        .reveal_plugin_token(SPAWN_ID, None)
        .await
        .unwrap_err();
    assert_eq!(auth_error_code(&error), Some(-32015));
    assert_eq!(survivor.whoami().await.unwrap(), "user");
    // -32015 后同一连接继续可用（与 -32014 的立即关闭不同）。
    assert_eq!(auth_conn.call("whoami", serde_json::json!({}), |_| {}).await.unwrap()["principal"], "user");
    // 核心其余能力照常：status 正常应答。
    let status = client.call("status", serde_json::json!({})).await.unwrap();
    assert!(status.is_object(), "身份缝停摆不得影响 status");

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn supervised_child_gets_fresh_token_per_spawn() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    // 每 1s 以非零退出触发退避重启，每代把当次注入的凭证写入 token.txt。
    install_supervised_package(
        &paths,
        SPAWN_ID,
        &format!(
            "#!/bin/sh\nprintf '%s' \"${}\" > token.txt\nprintf '%s' \"${}\" > data-dir.txt\nsleep 1\nexit 3\n",
            maild_brand::PLUGIN_TOKEN_ENV,
            maild_brand::PLUGIN_DATA_ENV
        ),
    );
    write_config(
        &paths,
        &format!(
            "[plugins.external.config]\nenabled = [\"{SPAWN_ID}\"]\nrestart_initial_secs = 1\nrestart_max_secs = 2\n"
        ),
    );

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let token_file = paths.plugin_dir.join(SPAWN_ID).join("token.txt");
    let data_dir_file = paths.plugin_dir.join(SPAWN_ID).join("data-dir.txt");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !token_file.exists() || !data_dir_file.exists() {
        assert!(tokio::time::Instant::now() < deadline, "子进程未在期限内拿到凭证");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let first = std::fs::read_to_string(&token_file).unwrap();
    assert!(first.starts_with("mdp_"));
    let injected_data_dir = std::fs::read_to_string(&data_dir_file).unwrap();
    assert_eq!(
        std::path::Path::new(&injected_data_dir),
        paths.plugin_data_dir.join(SPAWN_ID)
    );
    assert!(paths.plugin_data_dir.join(SPAWN_ID).is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(paths.plugin_data_dir.join(SPAWN_ID))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    // 等待下一次 spawn 轮换出不同凭证。
    let second = loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let current = std::fs::read_to_string(&token_file).unwrap_or_default();
        if !current.is_empty() && current != first {
            break current;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "重启后未观察到新代凭证"
        );
    };

    // 旧代凭证已被吊销，新一代可用。
    let client = unix_client(&socket);
    let mut stale_conn = client.connect_stream().await.unwrap();
    let error = stale_conn.authenticate(&first).await.unwrap_err();
    assert_eq!(auth_error_code(&error), Some(-32014));
    let mut fresh_conn = client.connect_stream().await.unwrap();
    fresh_conn.authenticate(&second).await.unwrap();
    let principal = fresh_conn.call("whoami", serde_json::json!({}), |_| {}).await.unwrap();
    assert_eq!(principal["principal"], format!("plugin:{SPAWN_ID}"));

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}
