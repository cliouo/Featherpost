//! M4-03 插件安装事务与生命周期验收（ADR-0019 实施切片）。
//!
//! 覆盖：崩溃残留 `.incoming`/`.removed` 不阻塞扫描；不停服 install → reveal
//! 立即可用 → enable 即 spawn → 低版本升级 -32017 且 staging 清理、高版本升级
//! 成功且 supervised child 以新代凭证重启 → rollback 回旧版（清单即时反映）→
//! remove 后清单消失且旧凭证吊销。脚本类用例限 Unix。

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

/// 模拟 CLI：把包复制进受管目录的 `.incoming/<staging>`（Agent 之外的唯一合法动作）。
#[cfg(unix)]
fn stage_supervised_package(root: &std::path::Path, staging: &str, version: &str, body: &str) {
    let id = TX_PLUGIN_ID;
    let dir = root.join(".incoming").join(staging);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Tx fixture\"\nversion = \"{version}\"\nlifecycle = \"supervised\"\ncapabilities = [\"messages_read\"]\nexecutable = \"bin/child.sh\"\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("bin/child.sh"), body).unwrap();
    seal_package(&dir);
    // supervised 可执行文件需要执行位（seal 之后单独放开）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            dir.join("bin/child.sh"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
}

fn stage_independent_package(root: &std::path::Path, staging: &str, id: &str, version: &str) {
    let dir = root.join(".incoming").join(staging);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Tx fixture\"\nversion = \"{version}\"\nlifecycle = \"independent\"\ncapabilities = [\"messages_read\"]\n"
        ),
    )
    .unwrap();
    seal_package(&dir);
}

fn stage_frontend_package(root: &std::path::Path, staging: &str, id: &str, version: &str) {
    let dir = root.join(".incoming").join(staging);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("frontend.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Tx frontend\"\nversion = \"{version}\"\nlifecycle = \"independent\"\ncapabilities = [\"compose\"]\n"
        ),
    )
    .unwrap();
    seal_package(&dir);
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

#[cfg(unix)]
async fn wait_until<F: FnMut() -> bool>(mut predicate: F, secs: u64, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "超时: {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn find_plugin<'a>(listing: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
    listing["plugins"].as_array()?.iter().find(|p| p["id"] == id)
}

#[cfg(unix)]
const TX_PLUGIN_ID: &str = "org.example.tx";

/// 全生命周期：install → enable → upgrade（拒绝/成功+换代）→ rollback → remove。
/// 崩溃残留目录在启动前预置，证明扫描与事务不受阻塞。
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn transaction_lifecycle_install_enable_upgrade_rollback_remove() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(
        &paths,
        "[plugins.external.config]\nrestart_initial_secs = 1\n",
    );
    // 崩溃残留：上一次运行的 staging 孤儿与移除暂存（点前缀必须被跳过）。
    let residue = paths.plugin_dir.join(".incoming").join("orphan001");
    std::fs::create_dir_all(&residue).unwrap();
    std::fs::write(residue.join("plugin.toml"), "schema_version = 1\n").unwrap();
    std::fs::create_dir_all(paths.plugin_dir.join(".removed").join("org.old-abc123")).unwrap();

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let user = unix_client(&socket);

    // 残留不进清单，事务照常可用。
    let listing = user.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    assert_eq!(listing["plugins"].as_array().map(Vec::len), Some(0));

    // 不停服 install v1.0.0（supervised + token.txt 记录每代注入凭证）。
    stage_supervised_package(
        &paths.plugin_dir,
        "stageaa11",
        "1.0.0",
        &format!(
            "#!/bin/sh\nprintf '%s' \"${}\" > token.txt\nsleep 30\n",
            maild_brand::PLUGIN_TOKEN_ENV
        ),
    );
    let outcome = user
        .call("commitPluginInstall", serde_json::json!({ "staging": "stageaa11" }))
        .await
        .unwrap();
    assert_eq!(outcome["action"], "installed");
    assert_eq!(outcome["id"], TX_PLUGIN_ID);
    assert_eq!(outcome["version"], "1.0.0");
    assert!(!paths.plugin_dir.join(".incoming/stageaa11").exists(), "发布后 staging 消失");

    // 身份条目随安装即时收敛：无需重启即可 reveal（has_identity=true）。
    let revealed = user
        .reveal_plugin_token(TX_PLUGIN_ID, None)
        .await
        .expect("安装后身份缝已就绪");
    assert!(revealed.starts_with("mdp_"));
    let listing = user.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    let entry = find_plugin(&listing, TX_PLUGIN_ID).unwrap();
    assert_eq!(entry["enabled"], false, "新装默认停用");
    assert_eq!(entry["has_identity"], true);

    // enable 即 running：子进程拿到注入凭证。
    user.call(
        "setPluginEnabled",
        serde_json::json!({ "id": TX_PLUGIN_ID, "enabled": true }),
    )
    .await
    .unwrap();
    let token_file = paths.plugin_dir.join(TX_PLUGIN_ID).join("token.txt");
    wait_until(|| token_file.exists(), 10, "enable 后子进程未 spawn").await;
    let first_gen = std::fs::read_to_string(&token_file).unwrap();
    assert!(first_gen.starts_with("mdp_"));
    let private_data_dir = paths.plugin_data_dir.join(TX_PLUGIN_ID);
    assert!(private_data_dir.is_dir(), "enable 前宿主已准备私有数据目录");
    let private_state = private_data_dir.join("state.txt");
    std::fs::write(&private_state, b"retained across package transactions").unwrap();

    // 低版本/同版本升级拒绝（-32017），坏请求不留现场。
    for rejected_version in ["0.9.0", "1.0.0"] {
        let staging_name = if rejected_version == "0.9.0" { "stagebb22" } else { "stagecc33" };
        stage_supervised_package(
            &paths.plugin_dir,
            staging_name,
            rejected_version,
            "#!/bin/sh\nsleep 30\n",
        );
        let error = user
            .call("commitPluginInstall", serde_json::json!({ "staging": staging_name }))
            .await
            .unwrap_err();
        assert_eq!(error.rpc_code, Some(-32017), "{rejected_version} 应被拒绝");
        assert!(
            !paths.plugin_dir.join(".incoming").join(staging_name).exists(),
            "被拒 staging 已清理"
        );
    }

    // 高版本升级成功：旧版入回滚位，child 因版本变化重启用新代凭证。
    stage_supervised_package(
        &paths.plugin_dir,
        "stagedd44",
        "1.1.0",
        &format!(
            "#!/bin/sh\nprintf '%s' \"${}\" > token.txt\nsleep 30\n",
            maild_brand::PLUGIN_TOKEN_ENV
        ),
    );
    let outcome = user
        .call("commitPluginInstall", serde_json::json!({ "staging": "stagedd44" }))
        .await
        .unwrap();
    assert_eq!(outcome["action"], "upgraded");
    assert_eq!(outcome["from_version"], "1.0.0");
    assert_eq!(outcome["version"], "1.1.0");
    assert!(paths.plugin_dir.join(".rollback").join(TX_PLUGIN_ID).is_dir(), "回滚位保留旧版");
    assert_eq!(
        std::fs::read(&private_state).unwrap(),
        b"retained across package transactions"
    );

    wait_until(
        || {
            std::fs::read_to_string(&token_file).unwrap_or_default()
                != first_gen
                && !std::fs::read_to_string(&token_file).unwrap_or_default().is_empty()
        },
        15,
        "升级后子进程未以新代凭证重启",
    )
    .await;
    let second_gen = std::fs::read_to_string(&token_file).unwrap();
    let mut stale = user.connect_stream().await.unwrap();
    let error = stale.authenticate(&first_gen).await.unwrap_err();
    assert_eq!(error.rpc_code, Some(-32014), "上一代凭证已被轮换吊销");
    let mut fresh = user.connect_stream().await.unwrap();
    fresh.authenticate(&second_gen).await.unwrap();
    assert_eq!(
        fresh.call("whoami", serde_json::json!({}), |_| {}).await.unwrap()["principal"],
        format!("plugin:{TX_PLUGIN_ID}")
    );

    // rollback 切回 1.0.0：清单版本即时反映，回滚位换成 1.1.0。
    let outcome = user
        .call("rollbackPlugin", serde_json::json!({ "id": TX_PLUGIN_ID }))
        .await
        .unwrap();
    assert_eq!(outcome["id"], TX_PLUGIN_ID);
    assert_eq!(outcome["version"], "1.0.0");
    let listing = user.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    assert_eq!(find_plugin(&listing, TX_PLUGIN_ID).unwrap()["version"], "1.0.0");
    assert_eq!(
        std::fs::read(&private_state).unwrap(),
        b"retained across package transactions"
    );

    // remove：出清单，身份条目收敛，最后一代凭证对新连接失效。
    user.call("removePlugin", serde_json::json!({ "id": TX_PLUGIN_ID })).await.unwrap();
    let listing = user.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    assert!(find_plugin(&listing, TX_PLUGIN_ID).is_none(), "移除后清单消失");
    assert!(!paths.plugin_dir.join(TX_PLUGIN_ID).exists());
    assert_eq!(
        std::fs::read(&private_state).unwrap(),
        b"retained across package transactions",
        "remove 只移除包，插件私有数据必须保留"
    );
    let mut revoked = user.connect_stream().await.unwrap();
    let error = revoked.authenticate(&second_gen).await.unwrap_err();
    assert_eq!(error.rpc_code, Some(-32014), "移除后凭证吊销");

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

/// 残留容错 + 参数卫生 + independent 启停 + frontend 命名空间事务。
#[tokio::test(flavor = "current_thread")]
async fn residue_tolerance_param_hygiene_and_frontend_transactions() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths, "[plugins.external.config]\n");
    // 崩溃残留：非法 manifest 的孤儿 staging 与陈旧 removed 目录。
    let residue = paths.plugin_dir.join(".incoming").join("junkjunk1");
    std::fs::create_dir_all(&residue).unwrap();
    std::fs::write(residue.join("plugin.toml"), "not = a valid manifest").unwrap();
    std::fs::create_dir_all(paths.frontend_dir.join(".removed").join("stale-xyz789")).unwrap();

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;
    let user = unix_client(&socket);

    // 残留不算清单成员。
    let listing = user.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    assert_eq!(listing["plugins"].as_array().map(Vec::len), Some(0));

    // 非法 staging 名 / 不存在的 staging：-32017，不影响后续事务。
    for bad in [
        serde_json::json!({ "staging": "../evil" }),
        serde_json::json!({ "staging": ".hidden1" }),
        serde_json::json!({ "staging": "nothere00" }),
        serde_json::json!({ "kind": "frontend", "staging": "../evil" }),
    ] {
        let error = user.call("commitPluginInstall", bad).await.unwrap_err();
        assert_eq!(error.rpc_code, Some(-32017));
    }

    // independent 包：install → enable → disable 全走状态文件。
    stage_independent_package(&paths.plugin_dir, "stagein01", "org.example.indep", "2.0.0");
    let outcome = user
        .call("commitPluginInstall", serde_json::json!({ "staging": "stagein01" }))
        .await
        .unwrap();
    assert_eq!(outcome["action"], "installed");
    user.call(
        "setPluginEnabled",
        serde_json::json!({ "id": "org.example.indep", "enabled": true }),
    )
    .await
    .unwrap();
    let listing = user.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    let entry = find_plugin(&listing, "org.example.indep").unwrap();
    assert_eq!(entry["enabled"], true);
    assert_eq!(entry["state"], "independent");
    user.call(
        "setPluginEnabled",
        serde_json::json!({ "id": "org.example.indep", "enabled": false }),
    )
    .await
    .unwrap();
    let listing = user.call("listExternalPlugins", serde_json::json!({})).await.unwrap();
    assert_eq!(find_plugin(&listing, "org.example.indep").unwrap()["enabled"], false);

    // frontend 命名空间：kind=frontend 走前端根目录，remove 后只剩内建。
    stage_frontend_package(&paths.frontend_dir, "stagefe01", "org.example.fe", "1.0.0");
    let outcome = user
        .call(
            "commitPluginInstall",
            serde_json::json!({ "kind": "frontend", "staging": "stagefe01" }),
        )
        .await
        .unwrap();
    assert_eq!(outcome["action"], "installed");
    let frontends = user.call("listFrontends", serde_json::json!({})).await.unwrap();
    assert!(
        frontends["frontends"].as_array().unwrap().iter().any(|f| f["id"] == "org.example.fe"),
        "前端包应出现在清单"
    );
    user
        .call(
            "removePlugin",
            serde_json::json!({ "kind": "frontend", "id": "org.example.fe" }),
        )
        .await
        .unwrap();
    let frontends = user.call("listFrontends", serde_json::json!({})).await.unwrap();
    assert!(
        !frontends["frontends"].as_array().unwrap().iter().any(|f| f["id"] == "org.example.fe"),
        "移除后前端清单收敛"
    );

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}
