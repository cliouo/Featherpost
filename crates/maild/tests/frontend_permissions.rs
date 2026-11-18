//! M4.5-G01 前端授权闭环验收（ADR-0030 实施切片）。
//!
//! 覆盖：只声明路由的前端拿不到写回与管理面；声明 `permissions` 后能完整承担日用主路径；
//! 提权防线在四词齐全时依然拦住 `revealPluginToken`；`prepareReply` 的纯读归类；
//! `permissions` 在 `listFrontends` 可审计；User 主体行为零变化。
//!
//! 本文件断言的是**声明与授权的对应关系**，不是沙箱。前端进程与用户同权，它随时可以
//! 开一条不认证的连接变回 `user`（取 token 时甚至必须这么做一次）。这条上限挡的是宿主
//! 代理层的 bug 与看不见的权限蔓延（ADR-0030 决策 1）。

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

fn write_config(paths: &AppPaths) -> std::path::PathBuf {
    for dir in [
        &paths.config_dir,
        &paths.data_dir,
        &paths.runtime_dir,
        &paths.plugin_dir,
        &paths.frontend_dir,
    ] {
        std::fs::create_dir_all(dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
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
        [plugins.frontend.config]
        default = "builtin-egui"
        [plugins.external.config]
        "#,
    )
    .unwrap();
    config
}

/// `permissions` 为 `None` 时写 v1 manifest，为 `Some` 时写 v2（ADR-0030 决策 3）。
fn install_frontend(paths: &AppPaths, id: &str, routes: &str, permissions: Option<&str>) {
    let package = paths.frontend_dir.join(id);
    std::fs::create_dir_all(&package).unwrap();
    let (schema_version, permissions) = match permissions {
        Some(values) => (2, format!("permissions = [{values}]\n")),
        None => (1, String::new()),
    };
    std::fs::write(
        package.join("frontend.toml"),
        format!(
            "schema_version = {schema_version}\nprotocol_version = 1\nid = \"{id}\"\nname = \"Permission fixture\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [{routes}]\n{permissions}"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            package.join("frontend.toml"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o700)).unwrap();
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

/// 决策 6 描述的 bootstrap：先以 user 取回并轮换自己的 token，再开一条连接认证。
async fn frontend_connection(client: &RpcClient, id: &str) -> maild_client::StreamConnection {
    let token = client
        .reveal_plugin_token(id, Some("frontend"))
        .await
        .expect("受管前端有身份条目");
    let mut connection = client.connect_stream().await.unwrap();
    connection.authenticate(&token).await.expect("认证成功");
    assert_eq!(
        connection
            .call("whoami", serde_json::json!({}), |_| {})
            .await
            .unwrap()["principal"],
        format!("frontend:{id}")
    );
    connection
}

async fn call_on(
    connection: &mut maild_client::StreamConnection,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, ClientError> {
    connection.call(method, params, |_| {}).await
}

/// 授权通过后请求进入领域层，失败原因是数据/依赖而不是授权。
///
/// 同时排除 -32601：否则一个拼错的方法名会静默「通过」——授权面的正向断言最容易这样烂掉。
fn assert_authorized(result: Result<serde_json::Value, ClientError>, method: &str) {
    if let Err(error) = result {
        assert_ne!(
            error.rpc_code,
            Some(-32016),
            "{method} 不应被授权拒绝（实际 {error:?}）"
        );
        assert_ne!(
            error.rpc_code,
            Some(-32601),
            "{method} 不存在——正向断言必须真的打到方法上"
        );
    }
}

async fn assert_denied(
    connection: &mut maild_client::StreamConnection,
    method: &str,
    params: serde_json::Value,
) {
    let error = call_on(connection, method, params)
        .await
        .unwrap_err_or_panic(method);
    assert_eq!(error.rpc_code, Some(-32016), "{method} 应被授权拒绝");
}

trait UnwrapErrOrPanic {
    fn unwrap_err_or_panic(self, method: &str) -> ClientError;
}

impl UnwrapErrOrPanic for Result<serde_json::Value, ClientError> {
    fn unwrap_err_or_panic(self, method: &str) -> ClientError {
        match self {
            Ok(value) => panic!("{method} 本应被拒绝，却返回 {value}"),
            Err(error) => error,
        }
    }
}

/// 一组「方法名 + 参数构造器」；参数用闭包而不是常量，因为 `serde_json::json!` 不是 const。
type MethodCase = (&'static str, fn() -> serde_json::Value);

const WRITE_METHODS: &[MethodCase] = &[
    ("markMessages", || {
        serde_json::json!({"account": "a", "uids": ["1"], "seen": true})
    }),
    ("moveMessages", || {
        serde_json::json!({"account": "a", "uids": ["1"], "target": "Archive"})
    }),
    ("deleteMessages", || {
        serde_json::json!({"account": "a", "uids": ["1"]})
    }),
    ("listPendingOps", || serde_json::json!({})),
    ("resolvePendingOp", || {
        serde_json::json!({"id": 1, "resolution": "discard"})
    }),
];

const MANAGEMENT_METHODS: &[MethodCase] = &[
    ("listAccountProfiles", || serde_json::json!({})),
    ("setAccountEnabled", || {
        serde_json::json!({"id": "a", "enabled": false})
    }),
    ("listAddresses", || serde_json::json!({})),
    ("listExternalPlugins", || serde_json::json!({})),
    ("refreshPlugins", || serde_json::json!({})),
];

#[tokio::test(flavor = "current_thread")]
async fn routes_alone_cannot_write_the_mailbox_or_manage_anything() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    // 一个「完整」的 v1 前端：全部路由，零 permissions。
    install_frontend(
        &paths,
        "org.example.routes",
        "\"inbox\", \"message\", \"compose\", \"settings\"",
        None,
    );
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let user = unix_client(&socket);
    let mut conn = frontend_connection(&user, "org.example.routes").await;

    // 路由翻译给到的四个词照旧可用。
    assert_authorized(
        call_on(&mut conn, "listMessageSummaries", serde_json::json!({"account": "a"})).await,
        "listMessageSummaries",
    );
    assert_authorized(call_on(&mut conn, "getSettings", serde_json::json!({})).await, "getSettings");

    // 写回与管理面一概不来自路由声明——这正是不并进 `message` 的理由（决策 2）。
    for (method, params) in WRITE_METHODS.iter().chain(MANAGEMENT_METHODS) {
        assert_denied(&mut conn, method, params()).await;
    }

    let denied = WRITE_METHODS.len() + MANAGEMENT_METHODS.len();
    let listing = user.call("listFrontends", serde_json::json!({})).await.unwrap();
    let entry = listing["frontends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "org.example.routes")
        .unwrap();
    assert_eq!(entry["denied_calls"], denied as u64);
    // v1 包的 permissions 为空表，且 additive 字段存在。
    assert_eq!(entry["permissions"], serde_json::json!([]));

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn declared_permissions_carry_the_daily_driver_path() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    install_frontend(
        &paths,
        "org.example.daily",
        "\"inbox\", \"message\", \"compose\", \"settings\"",
        Some("\"mailbox_write\", \"accounts_manage\", \"addresses_manage\", \"plugins_manage\""),
    );
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let user = unix_client(&socket);
    let mut conn = frontend_connection(&user, "org.example.daily").await;

    for (method, params) in WRITE_METHODS.iter().chain(MANAGEMENT_METHODS) {
        assert_authorized(call_on(&mut conn, method, params()).await, method);
    }
    // ADR-0030 决策 8 的三处归类修正。
    for (method, params) in [
        ("status", serde_json::json!({})),
        (
            "prepareReply",
            serde_json::json!({"account": "a", "uid": "1", "mode": "reply"}),
        ),
        (
            "sendFromAccount",
            serde_json::json!({"account": "a", "to": "c@d", "body": "x"}),
        ),
        ("requestNotificationAuthorization", serde_json::json!({})),
    ] {
        assert_authorized(call_on(&mut conn, method, params).await, method);
    }

    // 提权防线不因声明齐全而松动：持证者不得铸证（决策 6）。
    assert_denied(
        &mut conn,
        "revealPluginToken",
        serde_json::json!({"id": "org.example.daily", "kind": "frontend"}),
    )
    .await;

    // 声明可审计：用户装它之前能看见它要什么（决策 1）。
    let listing = user.call("listFrontends", serde_json::json!({})).await.unwrap();
    let entry = listing["frontends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "org.example.daily")
        .unwrap();
    assert_eq!(
        entry["permissions"],
        serde_json::json!([
            "accounts_manage",
            "addresses_manage",
            "mailbox_write",
            "plugins_manage"
        ])
    );
    assert_eq!(entry["denied_calls"], 1);

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

/// 决策 4/5：每个词只带自己的方法，`mailbox_write` 不顺带管理面。
#[tokio::test(flavor = "current_thread")]
async fn each_permission_carries_only_its_own_methods() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    install_frontend(
        &paths,
        "org.example.writer",
        "\"inbox\", \"message\"",
        Some("\"mailbox_write\""),
    );
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let user = unix_client(&socket);
    let mut conn = frontend_connection(&user, "org.example.writer").await;

    for (method, params) in WRITE_METHODS {
        assert_authorized(call_on(&mut conn, method, params()).await, method);
    }
    for (method, params) in MANAGEMENT_METHODS {
        assert_denied(&mut conn, method, params()).await;
    }
    // 没有 compose 路由就没有 messages_send：能标记不等于能发信。
    assert_denied(
        &mut conn,
        "send",
        serde_json::json!({"from": "a@b", "to": "c@d", "body": "x"}),
    )
    .await;
    // 但 prepareReply 只要 messages_read——拼草稿不是投递（决策 8）。
    assert_authorized(
        call_on(
            &mut conn,
            "prepareReply",
            serde_json::json!({"account": "a", "uid": "1", "mode": "reply"}),
        )
        .await,
        "prepareReply",
    );

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

/// 决策 3：v1 包声明 permissions 拒装（不降权静默装上），且不连累同目录的好包。
#[tokio::test(flavor = "current_thread")]
async fn a_v1_package_declaring_permissions_is_refused_without_breaking_neighbours() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    let package = paths.frontend_dir.join("org.example.v1perm");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("frontend.toml"),
        "schema_version = 1\nprotocol_version = 1\nid = \"org.example.v1perm\"\nname = \"Old\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"inbox\"]\npermissions = [\"mailbox_write\"]\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            package.join("frontend.toml"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    install_frontend(&paths, "org.example.good", "\"inbox\"", Some("\"mailbox_write\""));

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let user = unix_client(&socket);
    let listing = user.call("listFrontends", serde_json::json!({})).await.unwrap();
    let ids = listing["frontends"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(!ids.iter().any(|id| id == "org.example.v1perm"), "v1 带 permissions 应整包拒绝");
    assert!(ids.iter().any(|id| id == "org.example.good"));
    // 被拒的包没有身份条目，连认证都无从谈起。
    assert!(user
        .reveal_plugin_token("org.example.v1perm", Some("frontend"))
        .await
        .is_err());

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

/// ADR-0030 决策 11 / ADR-0031 决策 8：前端主体隐含 frontend_open，但只能 claim 自己的 intent；
/// `notifications_read` 只有显式声明才拿得到。
#[tokio::test(flavor = "current_thread")]
async fn a_frontend_claims_only_its_own_intents_and_reads_notifications_when_declared() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    install_frontend(&paths, "org.example.shell", "\"inbox\", \"message\"", None);
    install_frontend(
        &paths,
        "org.example.notify",
        "\"settings\"",
        Some("\"notifications_read\""),
    );
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let user = unix_client(&socket);
    // user 先给两个前端各排一条它们支持的 intent（openFrontend 按路由声明校验）。
    for (frontend, route) in [("org.example.shell", "inbox"), ("org.example.notify", "settings")] {
        user.call(
            "openFrontend",
            serde_json::json!({"frontend": frontend, "intent": {"route": route}}),
        )
        .await
        .unwrap();
    }

    let mut shell = frontend_connection(&user, "org.example.shell").await;
    // 隐含的 frontend_open：读清单、claim 自己的 intent 都通。
    let listing = call_on(&mut shell, "listFrontends", serde_json::json!({})).await.unwrap();
    assert!(listing["frontends"].as_array().unwrap().len() >= 2);
    let claimed = call_on(
        &mut shell,
        "claimNextOpenIntent",
        serde_json::json!({"frontend": "org.example.shell"}),
    )
    .await
    .unwrap();
    assert_eq!(claimed["intent"]["route"], "inbox");
    // 但替别的前端 claim 是参数错误，不是授权错误——授权已过，越界的是对象。
    let error = call_on(
        &mut shell,
        "claimNextOpenIntent",
        serde_json::json!({"frontend": "org.example.notify"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32602));
    // 被拒的那条 intent 仍然留给它的主人。
    let mut notify = frontend_connection(&user, "org.example.notify").await;
    let claimed = call_on(
        &mut notify,
        "claimNextOpenIntent",
        serde_json::json!({"frontend": "org.example.notify"}),
    )
    .await
    .unwrap();
    assert_eq!(claimed["intent"]["route"], "settings");

    // notifications_read：声明的能读，没声明的（shell 只有 inbox/message 路由）被拒。
    assert_authorized(
        call_on(&mut notify, "getNotificationStatus", serde_json::json!({})).await,
        "getNotificationStatus",
    );
    assert_denied(&mut shell, "getNotificationStatus", serde_json::json!({})).await;

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

/// User 主体行为零变化：能打开 socket 的就是机主（决策「非目标」第一条）。
#[tokio::test(flavor = "current_thread")]
async fn the_user_principal_is_untouched() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let user = unix_client(&socket);
    for (method, params) in WRITE_METHODS.iter().chain(MANAGEMENT_METHODS) {
        assert_authorized(user.call(method, params()).await, method);
    }
    assert_authorized(user.call("status", serde_json::json!({})).await, "status");
    // 未认证连接依然能取凭证——这正是决策 6 说 bootstrap 无法消除的地方。
    assert!(user
        .reveal_plugin_token("builtin-egui", Some("frontend"))
        .await
        .is_err(), "builtin 不参与逐插件身份");

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}
