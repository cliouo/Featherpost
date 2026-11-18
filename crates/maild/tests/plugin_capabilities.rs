//! M4-02 方法级 capability enforcement 验收（ADR-0018 实施切片）。
//!
//! 覆盖：声明 `messages_read` 的插件可读信但发信/用户域方法 -32016 且计数递增、
//! 提权防线（revealPluginToken/账户方法对插件主体拒绝）、前端 route 翻译授权、
//! User 主体行为零变化。推送过滤的判定由 api/frontend 单测钉死。

use std::time::Duration;

use maild::{run_daemon_until, DaemonOptions};
use maild_client::{ClientError, Endpoint, RpcClient};
use maild_paths::AppPaths;

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

fn install_external_package(paths: &AppPaths, id: &str, capabilities: &str) {
    let package = paths.plugin_dir.join(id);
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("plugin.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Cap fixture\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [{capabilities}]\n"
        ),
    )
    .unwrap();
    seal_package(&package);
}

fn install_frontend_package(paths: &AppPaths, id: &str, capabilities: &str) {
    let package = paths.frontend_dir.join(id);
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("frontend.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Route fixture\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [{capabilities}]\n"
        ),
    )
    .unwrap();
    seal_package(&package);
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

/// 认证一条长连接并断言主体绑定成功。
async fn authenticated_connection(
    client: &RpcClient,
    token: &str,
) -> maild_client::StreamConnection {
    let mut connection = client.connect_stream().await.unwrap();
    connection.authenticate(token).await.expect("认证成功");
    connection
}

async fn call_on(
    connection: &mut maild_client::StreamConnection,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, ClientError> {
    connection.call(method, params, |_| {}).await
}

const PLUGIN_ID: &str = "org.example.reader";

#[tokio::test(flavor = "current_thread")]
async fn declared_capability_allowed_and_rest_denied_with_counter() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths, "[plugins.external.config]\n");
    install_external_package(&paths, PLUGIN_ID, "\"messages_read\"");
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let user = unix_client(&socket);
    let token = user.reveal_plugin_token(PLUGIN_ID, None).await.unwrap();
    let mut conn = authenticated_connection(&user, &token).await;

    // 声明过的 messages_read：请求到达领域层（store 无此邮件 → -32002，不是 -32016）。
    let error = call_on(&mut conn, "getMessage", serde_json::json!({"account": "a", "uid": "1"}))
        .await
        .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32002), "授权通过后进入领域语义");

    // ADR-0030 决策 8：`status` 归 messages_read——它给的版本/账户状态/落库计数，
    // `getAgentInfo` 与 `listAccounts` 已经给了，留在 UserOnly 只是少一个诊断入口。
    assert!(call_on(&mut conn, "status", serde_json::json!({})).await.is_ok());

    // 未声明的能力与用户域一律 -32016。
    for (method, params) in [
        ("send", serde_json::json!({"from": "a@b", "to": "c@d", "body": "x"})),
        ("listExternalPlugins", serde_json::json!({})),
        ("revealPluginToken", serde_json::json!({"id": PLUGIN_ID})),
        ("listAccountProfiles", serde_json::json!({})),
        ("sendFromAccount", serde_json::json!({"account": "a", "to": "c@d"})),
        // ADR-0030 决策 5：信箱写回有自己的词，可读主体拿不到它。
        ("markMessages", serde_json::json!({"account": "a", "uids": ["1"], "seen": true})),
        ("deleteMessages", serde_json::json!({"account": "a", "uids": ["1"]})),
    ] {
        let error = call_on(&mut conn, method, params).await.unwrap_err();
        assert_eq!(error.rpc_code, Some(-32016), "{method} 应被拒绝");
    }
    // 连接未断开：-32016 与 -32014 语义分离。
    assert_eq!(
        call_on(&mut conn, "whoami", serde_json::json!({})).await.unwrap()["principal"],
        format!("plugin:{PLUGIN_ID}")
    );
    // Open 方法不受限。
    assert!(call_on(&mut conn, "getAgentInfo", serde_json::json!({})).await.is_ok());

    // 拒绝计数按主体可见（上表共 7 次拒绝）。
    let listing = user.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    assert_eq!(listing["plugins"][0]["denied_calls"], 7);

    // User 主体零变化：同样的调用照常到达领域层。
    assert!(user.call("status", serde_json::json!({})).await.is_ok());
    let error = user
        .call("send", serde_json::json!({"from": "a@b", "to": "c@d", "body": "x"}))
        .await
        .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32001), "user 走到依赖缺失而非授权拒绝");

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn frontend_routes_translate_to_api_capabilities() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths, "[plugins.external.config]\n");
    install_frontend_package(&paths, "org.example.composeui", "\"compose\"");
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let user = unix_client(&socket);
    let token = user
        .reveal_plugin_token("org.example.composeui", Some("frontend"))
        .await
        .unwrap();
    let mut conn = authenticated_connection(&user, &token).await;
    assert_eq!(
        call_on(&mut conn, "whoami", serde_json::json!({})).await.unwrap()["principal"],
        "frontend:org.example.composeui"
    );

    // compose → messages_send：send 到达依赖层（transport 未装配 → -32001）。
    let error = call_on(
        &mut conn,
        "send",
        serde_json::json!({"from": "a@b", "to": "c@d", "body": "x"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32001));
    // compose 不含 messages_read。
    let error = call_on(
        &mut conn,
        "getMessage",
        serde_json::json!({"account": "a", "uid": "1"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32016));

    // 计数落在 frontend 命名空间。
    let listing = user.call("listFrontends", serde_json::json!({})).await.unwrap();
    let fixture = listing["frontends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "org.example.composeui")
        .unwrap()
        .clone();
    assert_eq!(fixture["denied_calls"], 1);

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}
