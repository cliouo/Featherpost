//! ADR-0050 PR 1：外部翻译 provider 总线全量 hermetic 验收测试。
//!
//! 全部在真实 Unix socket IPC 流上运行：
//! 1. 同连接两批乱序并发翻译（Batch B 先于 Batch A 完成，ID 原样回显）
//! 2. 慢翻译在途期间同连接并发读方法（status）与 `cancelTranslation`（立即可取消且向插件下发 `translation/cancel`）
//! 3. 跨会话 `cancelTranslation` 被拒（-32016）
//! 4. 端点并发与排队满载返回 `-32022`（busy）
//! 5. 双角色连接（`transport_provider` + `translation_provider`）单角色顶替不误伤另一角色
//! 6. 迟到应答被静默丢弃且绝不触发 `-32600` 错误帧
//! 7. 强制断连 / EOF 使在途翻译立返 `-32018` 且广播 `translation/providers`
//! 8. 完整生命周期：install → enable → register → probe/invoke → disable（含 independent 停用撤销）→ upgrade → rollback → remove
//! 9. `builtin.*` manifest ID 在安装事务与目录校验被拒
//! 10. 主体/来源权限边界：非 Studio 前端、插件、Unix User、HTTP User 在 invoke/cancel/test 均被拒，推送按 `plugins_manage` 过滤。

#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use maild::{run_daemon_until, DaemonOptions};
use maild_client::{
    stage_plugin_package_into, ClientError, Endpoint, Incoming, JsonRpcError, RpcClient,
    StreamConnection, TranslationProvidersResult, PROVIDER_BUSY_CODE, PROVIDER_CANCELLED_CODE,
    PROVIDER_TIMEOUT_CODE, PROVIDER_UNAVAILABLE_CODE, TRANSLATION_PROVIDERS_NOTIFICATION,
};
use maild_paths::AppPaths;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch, Notify};

fn test_paths(root: &Path) -> AppPaths {
    AppPaths::from_roots(
        &root.join("config"),
        &root.join("data"),
        Some(&root.join("runtime")),
        Some(&root.join("state")),
        root,
    )
}

fn chmod_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn chmod_file(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn install_frontend_package(paths: &AppPaths, id: &str, permissions: &str) {
    let package = paths.frontend_dir.join(id);
    chmod_dir(&package);
    let manifest = package.join("frontend.toml");
    std::fs::write(
        &manifest,
        format!(
            "schema_version = 3\nprotocol_version = 1\nid = \"{id}\"\nname = \"Frontend {id}\"\nversion = \"0.4.0\"\nlifecycle = \"independent\"\ncapabilities = [\"inbox\", \"message\", \"settings\"]\npermissions = [{permissions}]\n"
        ),
    )
    .unwrap();
    chmod_file(&manifest);
}

fn install_external_v2_package(
    root: &Path,
    id: &str,
    version: &str,
    capabilities: &str,
) -> PathBuf {
    let package = root.join(id);
    chmod_dir(&package);
    let manifest = package.join("plugin.toml");
    std::fs::write(
        &manifest,
        format!(
            "schema_version = 2\nprotocol_version = 1\nid = \"{id}\"\nname = \"Translator {id}\"\ndescription = \"Hermetic test translator\"\nversion = \"{version}\"\nlifecycle = \"independent\"\ncapabilities = [{capabilities}]\n[l10n.\"zh-Hans\"]\nname = \"测试翻译器\"\n"
        ),
    )
    .unwrap();
    chmod_file(&manifest);
    package
}

fn write_daemon_config(paths: &AppPaths, extra: &str) {
    chmod_dir(&paths.config_dir);
    chmod_dir(&paths.data_dir);
    chmod_dir(&paths.runtime_dir);
    chmod_dir(&paths.plugin_dir);
    chmod_dir(&paths.frontend_dir);
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
            default = "dev.featherpost.studio"
            [plugins.external.config]
            {extra}
            "#
        ),
    )
    .unwrap();
    chmod_file(&config);
}

type DaemonTask = tokio::task::JoinHandle<Result<(), maild::DaemonError>>;

fn spawn_daemon(paths: &AppPaths) -> (DaemonTask, oneshot::Sender<()>) {
    let options = DaemonOptions::new(paths.config_file.clone(), paths.clone()).unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(run_daemon_until(options, async move {
        let _ = shutdown_rx.await;
    }));
    (task, shutdown_tx)
}

async fn wait_socket(task: &mut DaemonTask, socket: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        if tokio::time::Instant::now() >= deadline {
            let err = (&mut *task).await.unwrap().err();
            panic!("socket 未就绪: {err:?}");
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn unix_rpc(socket: &Path) -> RpcClient {
    RpcClient::new(Endpoint::Unix {
        path: socket.to_path_buf(),
    })
    .unwrap()
}

async fn connect_authenticated(socket: &Path, kind: &str, id: &str) -> StreamConnection {
    let token = unix_rpc(socket)
        .call("revealPluginToken", json!({ "kind": kind, "id": id }))
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut conn = maild_client::connect_stream(&Endpoint::Unix {
        path: socket.to_path_buf(),
    })
    .await
    .unwrap();
    conn.authenticate(&token).await.unwrap();
    conn
}

async fn stop_daemon(task: DaemonTask, shutdown_tx: oneshot::Sender<()>) {
    let _ = shutdown_tx.send(());
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

type RpcReplyTx = oneshot::Sender<Result<Value, JsonRpcError>>;
type RpcCommand = (String, Value, RpcReplyTx);

/// 并发请求/响应收集器：把真实 Studio IPC 流拆成读写两侧，支持同连接并发发多条请求并按 ID 等待乱序应答。
struct StudioStreamClient {
    cmd_tx: mpsc::Sender<RpcCommand>,
    push_rx: mpsc::Receiver<TranslationProvidersResult>,
    _task: tokio::task::JoinHandle<()>,
}

impl StudioStreamClient {
    fn from_conn(conn: StreamConnection) -> Self {
        let (mut reader, mut writer) = conn.into_split();
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<RpcCommand>(32);
        let (push_tx, push_rx) = mpsc::channel::<TranslationProvidersResult>(32);

        let task = tokio::spawn(async move {
            let mut pending: HashMap<String, RpcReplyTx> = HashMap::new();
            loop {
                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        let Some((method, params, reply)) = cmd else { break; };
                        match writer.send_request(&method, params).await {
                            Ok(id) => {
                                pending.insert(id.to_string(), reply);
                            }
                            Err(err) => {
                                let _ = reply.send(Err(JsonRpcError {
                                    code: -32000,
                                    message: err.to_string(),
                                    data: None,
                                }));
                                break;
                            }
                        }
                    }
                    incoming = reader.next_incoming() => {
                        match incoming {
                            Ok(Incoming::Response { id, result }) => {
                                if let Some(tx) = pending.remove(&id.to_string()) {
                                    let _ = tx.send(result);
                                }
                            }
                            Ok(Incoming::Notification(n)) => {
                                if let Some(Ok(ev)) = n.translation_providers_changed() {
                                    let _ = push_tx.try_send(ev);
                                }
                            }
                            Ok(Incoming::Request { .. }) => {}
                            Err(_) => break,
                        }
                    }
                }
            }
        });
        Self {
            cmd_tx,
            push_rx,
            _task: task,
        }
    }

    async fn send(&self, method: &str, params: Value) -> oneshot::Receiver<Result<Value, JsonRpcError>> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send((method.to_owned(), params, tx))
            .await
            .unwrap();
        rx
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, JsonRpcError> {
        self.send(method, params).await.await.unwrap()
    }

    async fn next_providers_push(&mut self) -> TranslationProvidersResult {
        tokio::time::timeout(Duration::from_secs(3), self.push_rx.recv())
            .await
            .expect("等待 translation/providers 推送超时")
            .expect("推送通道已关闭")
    }
}

#[tokio::test]
async fn concurrent_out_of_order_batches_non_blocking_read_and_cancel_on_same_ipc_stream() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(dir.path());
    write_daemon_config(&paths, "");
    install_frontend_package(
        &paths,
        maild_brand::BUNDLED_FRONTEND_ID,
        "\"plugins_manage\"",
    );
    install_external_v2_package(
        &paths.plugin_dir,
        "org.example.tr",
        "1.0.0",
        "\"translation_provider\"",
    );

    let (mut daemon, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket(&mut daemon, &socket).await;

    unix_rpc(&socket)
        .call(
            "setPluginEnabled",
            json!({ "id": "org.example.tr", "enabled": true }),
        )
        .await
        .unwrap();

    // 1) 启动一个非阻塞测试翻译插件（max_concurrency=2, supports_cancel=true）：
    //    - batch_id == "slow" 会等待 release 信号或 cancel 通知；
    //    - batch_id == "fast" 立即返回。
    let mut plugin_conn = connect_authenticated(&socket, "plugin", "org.example.tr").await;
    let reg = plugin_conn
        .call(
            "translationProviderRegister",
            json!({
                "supported_languages": ["en", "zh-Hans"],
                "max_batch_items": 8,
                "max_batch_chars": 4000,
                "max_concurrency": 2,
                "supports_cancel": true,
                "variant_revision": "rev-1"
            }),
            |_| {},
        )
        .await
        .unwrap();
    let generation = reg["generation"].as_u64().unwrap();
    assert!(generation >= 1);

    let release_slow = Arc::new(Notify::new());
    let slow_started = Arc::new(Notify::new());
    let cancel_received = Arc::new(Notify::new());
    let cancelled_ids: Arc<StdMutex<Vec<Value>>> = Arc::new(StdMutex::new(Vec::new()));

    let plugin_task = {
        let release_slow = Arc::clone(&release_slow);
        let slow_started = Arc::clone(&slow_started);
        let cancel_received = Arc::clone(&cancel_received);
        let cancelled_ids = Arc::clone(&cancelled_ids);
        let (mut reader, mut writer) = plugin_conn.into_split();
        let (reply_tx, mut reply_rx) = mpsc::channel::<(Value, Result<Value, JsonRpcError>)>(16);
        tokio::spawn(async move {
            while let Some((id, outcome)) = reply_rx.recv().await {
                if writer.respond(&id, outcome).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let cancels: Arc<StdMutex<HashMap<String, watch::Sender<bool>>>> =
                Arc::new(StdMutex::new(HashMap::new()));
            while let Ok(frame) = reader.next_incoming().await {
                match frame {
                    Incoming::Request { id, method, params } if method == "translation/translate" => {
                        let batch_id = params
                            .get("batch_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        let reply_tx = reply_tx.clone();
                        let release_slow = Arc::clone(&release_slow);
                        let slow_started = Arc::clone(&slow_started);
                        let cancels = Arc::clone(&cancels);
                        let id_key = id.to_string();
                        let (c_tx, mut c_rx) = watch::channel(false);
                        cancels.lock().unwrap().insert(id_key.clone(), c_tx);
                        tokio::spawn(async move {
                            if batch_id.starts_with("slow") {
                                slow_started.notify_one();
                                tokio::select! {
                                    _ = release_slow.notified() => {}
                                    _ = async {
                                        while !*c_rx.borrow() {
                                            if c_rx.changed().await.is_err() {
                                                break;
                                            }
                                        }
                                    } => {
                                        cancels.lock().unwrap().remove(&id_key);
                                        return;
                                    }
                                }
                            }
                            cancels.lock().unwrap().remove(&id_key);
                            let items: Vec<Value> = params["items"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|item| {
                                    json!({
                                        "id": item["id"],
                                        "text": format!("TR:{}", item["text"].as_str().unwrap())
                                    })
                                })
                                .collect();
                            let _ = reply_tx
                                .send((
                                    id,
                                    Ok(json!({
                                        "items": items,
                                        "detected_source_lang": "en",
                                        "variant_revision": "rev-1"
                                    })),
                                ))
                                .await;
                        });
                    }
                    Incoming::Notification(n) if n.method == "translation/cancel" => {
                        let req_id = n.params["request_id"].clone();
                        cancelled_ids.lock().unwrap().push(req_id.clone());
                        if let Some(tx) = cancels.lock().unwrap().remove(&req_id.to_string()) {
                            tx.send_replace(true);
                        }
                        cancel_received.notify_one();
                    }
                    _ => {}
                }
            }
        })
    };

    // 2) 连接受托 Studio 会话 1 与会话 2
    let studio1 = StudioStreamClient::from_conn(
        connect_authenticated(&socket, "frontend", maild_brand::BUNDLED_FRONTEND_ID).await,
    );
    let studio2 = StudioStreamClient::from_conn(
        connect_authenticated(&socket, "frontend", maild_brand::BUNDLED_FRONTEND_ID).await,
    );

    // —— 场景 A：同连接两批乱序完成 ——
    let rx_slow = studio1
        .send(
            "invokeTranslation",
            json!({
                "provider_id": "org.example.tr",
                "generation": generation,
                "job_id": "job-ooo",
                "batch_id": "slow-1",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "items": [{ "id": 1, "text": "first" }]
            }),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(2), slow_started.notified())
        .await
        .unwrap();

    // 在 slow-1 仍在飞时，同连接发出 fast-2 与 status！
    let rx_fast = studio1
        .send(
            "invokeTranslation",
            json!({
                "provider_id": "org.example.tr",
                "generation": generation,
                "job_id": "job-ooo",
                "batch_id": "fast-2",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "items": [{ "id": 2, "text": "second" }]
            }),
        )
        .await;
    let status_res = tokio::time::timeout(Duration::from_secs(2), studio1.call("status", json!({})))
        .await
        .expect("慢翻译期间同连接调用 status 不应阻塞")
        .unwrap();
    assert!(status_res.get("version").is_some());

    // Batch B（fast-2）先于 Batch A（slow-1）完成！
    let fast_res = tokio::time::timeout(Duration::from_secs(2), rx_fast)
        .await
        .expect("fast 批次应先于 slow 批次完成")
        .unwrap()
        .unwrap();
    assert_eq!(fast_res["items"][0]["id"], 2);
    assert_eq!(fast_res["items"][0]["text"], "TR:second");

    // 放行 slow-1
    release_slow.notify_one();
    let slow_res = tokio::time::timeout(Duration::from_secs(2), rx_slow)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(slow_res["items"][0]["id"], 1);
    assert_eq!(slow_res["items"][0]["text"], "TR:first");

    // —— 场景 B：慢翻译期间跨会话取消被拒 + 同会话取消成功并通知插件 ——
    let rx_cancel_target = studio1
        .send(
            "invokeTranslation",
            json!({
                "provider_id": "org.example.tr",
                "generation": generation,
                "job_id": "job-cancel-me",
                "batch_id": "slow-cancel",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "items": [{ "id": 10, "text": "cancelable" }]
            }),
        )
        .await;
    tokio::time::timeout(Duration::from_secs(2), slow_started.notified())
        .await
        .unwrap();

    // 跨会话取消必须被拒（-32016）！
    let cross_err = studio2
        .call("cancelTranslation", json!({ "job_id": "job-cancel-me" }))
        .await
        .unwrap_err();
    assert_eq!(cross_err.code, maild::api::METHOD_NOT_PERMITTED);

    // 同会话取消立即生效且向插件发送 translation/cancel
    let cancel_ok = tokio::time::timeout(
        Duration::from_secs(2),
        studio1.call("cancelTranslation", json!({ "job_id": "job-cancel-me" })),
    )
    .await
    .expect("cancelTranslation 不应被慢翻译阻塞")
    .unwrap();
    assert_eq!(cancel_ok["cancelled"], true);
    assert_eq!(cancel_ok["cancelled_requests"], 1);
    assert_eq!(cancel_ok["notified_provider"], 1);

    let cancelled_call_err = tokio::time::timeout(Duration::from_secs(2), rx_cancel_target)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(cancelled_call_err.code, PROVIDER_CANCELLED_CODE);

    tokio::time::timeout(Duration::from_secs(2), cancel_received.notified())
        .await
        .expect("插件必须收到 translation/cancel 通知");
    assert_eq!(cancelled_ids.lock().unwrap().len(), 1);

    plugin_task.abort();
    stop_daemon(daemon, shutdown).await;
}

#[tokio::test]
async fn queue_full_returns_busy_and_late_response_is_silently_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(dir.path());
    write_daemon_config(&paths, "");
    install_frontend_package(
        &paths,
        maild_brand::BUNDLED_FRONTEND_ID,
        "\"plugins_manage\"",
    );
    install_external_v2_package(
        &paths.plugin_dir,
        "org.example.busy",
        "1.0.0",
        "\"translation_provider\"",
    );

    let (mut daemon, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket(&mut daemon, &socket).await;
    unix_rpc(&socket)
        .call(
            "setPluginEnabled",
            json!({ "id": "org.example.busy", "enabled": true }),
        )
        .await
        .unwrap();

    // max_concurrency = 1 → 1 个执行 + MAX_ENDPOINT_QUEUED(4) 个排队 = 5 个，第 6 个立返 busy (-32022)
    let mut plugin_conn = connect_authenticated(&socket, "plugin", "org.example.busy").await;
    let reg = plugin_conn
        .call(
            "translationProviderRegister",
            json!({
                "max_concurrency": 1,
                "max_batch_items": 4,
                "max_batch_chars": 1000,
                "supports_cancel": false
            }),
            |_| {},
        )
        .await
        .unwrap();
    let generation = reg["generation"].as_u64().unwrap();

    let (mut p_reader, mut p_writer) = plugin_conn.into_split();
    let studio = StudioStreamClient::from_conn(
        connect_authenticated(&socket, "frontend", maild_brand::BUNDLED_FRONTEND_ID).await,
    );

    // 1) 超时 + 迟到应答：先发一条 timeout_ms=80 的请求，让它超时得 -32019
    let timed_out_rx = studio
        .send(
            "invokeTranslation",
            json!({
                "provider_id": "org.example.busy",
                "generation": generation,
                "job_id": "job-timeout",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "timeout_ms": 80,
                "items": [{ "id": 1, "text": "hello" }]
            }),
        )
        .await;
    let Incoming::Request { id: timed_out_id, .. } =
        tokio::time::timeout(Duration::from_secs(2), p_reader.next_incoming())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("expected translation/translate request");
    };
    let timeout_err = tokio::time::timeout(Duration::from_secs(2), timed_out_rx)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(timeout_err.code, PROVIDER_TIMEOUT_CODE);

    // 现在插件把迟到应答发回 daemon：daemon 必须静默丢弃，不得回发任何 error 帧！
    p_writer
        .respond(
            &timed_out_id,
            Ok(json!({ "items": [{ "id": 1, "text": "late" }] })),
        )
        .await
        .unwrap();

    // 等一小段时间，确认插件连接上没有收到任何错误帧（如 -32600）
    let unexpected = tokio::time::timeout(Duration::from_millis(120), p_reader.next_incoming()).await;
    assert!(
        unexpected.is_err(),
        "迟到应答绝不能触发错误帧回发，实际收到: {unexpected:?}"
    );

    // 2) 队满返回 busy（-32022）：
    // 发出 1 + MAX_ENDPOINT_QUEUED (5) 个请求占满端点，再发第 6 个立返 busy
    let capacity = 1 + maild::translation_provider::MAX_ENDPOINT_QUEUED;
    let mut held_receivers = Vec::new();
    for idx in 0..capacity {
        held_receivers.push(
            studio
                .send(
                    "invokeTranslation",
                    json!({
                        "provider_id": "org.example.busy",
                        "generation": generation,
                        "job_id": format!("job-fill-{idx}"),
                        "source_lang": "en",
                        "target_lang": "zh-Hans",
                        "content_kind": "plain",
                        "items": [{ "id": 1, "text": "hold" }]
                    }),
                )
                .await,
        );
    }
    // 等第 1 个请求抵达插件（确保前 5 个已入队）
    let Incoming::Request {
        id: first_held_id, ..
    } = tokio::time::timeout(Duration::from_secs(2), p_reader.next_incoming())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected held request");
    };
    tokio::time::sleep(Duration::from_millis(50)).await;

    let busy_err = studio
        .call(
            "invokeTranslation",
            json!({
                "provider_id": "org.example.busy",
                "generation": generation,
                "job_id": "job-overflow",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "items": [{ "id": 1, "text": "overflow" }]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(busy_err.code, PROVIDER_BUSY_CODE);
    assert_eq!(busy_err.data.unwrap()["reason"], "busy");

    // 协商批次上限：插件声明 max_batch_items=4，发 5 条必须被拒（-32602）
    let over_batch = studio
        .call(
            "invokeTranslation",
            json!({
                "provider_id": "org.example.busy",
                "generation": generation,
                "job_id": "job-batch-limit",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "items": [
                    { "id": 1, "text": "a" },
                    { "id": 2, "text": "b" },
                    { "id": 3, "text": "c" },
                    { "id": 4, "text": "d" },
                    { "id": 5, "text": "e" }
                ]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(over_batch.code, maild::api::INVALID_PARAMS);

    // 3) 强制断开插件连接（EOF）→ 所有在途与排队的调用立即失败 -32018！
    let _ = first_held_id;
    drop(p_reader);
    drop(p_writer);
    for rx in held_receivers {
        let err = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, PROVIDER_UNAVAILABLE_CODE);
    }

    stop_daemon(daemon, shutdown).await;
}

#[tokio::test]
async fn dual_role_connection_replacement_does_not_break_transport() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(dir.path());
    write_daemon_config(&paths, "");
    install_frontend_package(
        &paths,
        maild_brand::BUNDLED_FRONTEND_ID,
        "\"plugins_manage\", \"accounts_manage\"",
    );
    install_external_v2_package(
        &paths.plugin_dir,
        "org.example.dual",
        "1.0.0",
        "\"transport_provider\", \"translation_provider\"",
    );

    let (mut daemon, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket(&mut daemon, &socket).await;
    unix_rpc(&socket)
        .call(
            "setPluginEnabled",
            json!({ "id": "org.example.dual", "enabled": true }),
        )
        .await
        .unwrap();

    // 连接 A 同时注册 transport_provider 与 translation_provider
    let mut conn_a = connect_authenticated(&socket, "plugin", "org.example.dual").await;
    conn_a.call("providerRegister", json!({}), |_| {}).await.unwrap();
    let reg_a = conn_a
        .call("translationProviderRegister", json!({}), |_| {})
        .await
        .unwrap();
    let gen_a = reg_a["generation"].as_u64().unwrap();

    // 连接 B 仅顶替 translation_provider（绝不能破坏连接 A 的 transport_provider！）
    let mut conn_b = connect_authenticated(&socket, "plugin", "org.example.dual").await;
    let reg_b = conn_b
        .call("translationProviderRegister", json!({}), |_| {})
        .await
        .unwrap();
    let gen_b = reg_b["generation"].as_u64().unwrap();
    assert!(gen_b > gen_a);

    // 在连接 A 上持续响应 transport/probe（证明 transport 角色完全完好）
    let (mut a_reader, mut a_writer) = conn_a.into_split();
    let transport_task = tokio::spawn(async move {
        while let Ok(frame) = a_reader.next_incoming().await {
            if let Incoming::Request { id, method, .. } = frame {
                assert_eq!(method, "transport/probe");
                a_writer
                    .respond(&id, Ok(json!({ "ok": true, "stage": "provider", "detail": "alive" })))
                    .await
                    .unwrap();
            }
        }
    });

    // 在连接 B 上持续响应 translation/probe
    let (mut b_reader, mut b_writer) = conn_b.into_split();
    let translation_task = tokio::spawn(async move {
        while let Ok(frame) = b_reader.next_incoming().await {
            if let Incoming::Request { id, method, .. } = frame {
                assert_eq!(method, "translation/probe");
                b_writer
                    .respond(&id, Ok(json!({ "ok": true, "detail": "conn-b-ok" })))
                    .await
                    .unwrap();
            }
        }
    });

    let studio = StudioStreamClient::from_conn(
        connect_authenticated(&socket, "frontend", maild_brand::BUNDLED_FRONTEND_ID).await,
    );

    // 旧代次 gen_a 调用翻译被拒 -32018
    let old_gen_err = studio
        .call(
            "invokeTranslation",
            json!({
                "provider_id": "org.example.dual",
                "generation": gen_a,
                "job_id": "job-stale-gen",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "items": [{ "id": 1, "text": "hi" }]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(old_gen_err.code, PROVIDER_UNAVAILABLE_CODE);

    // 新代次 gen_b 探测成功
    let tr_probe = studio
        .call(
            "testTranslationProvider",
            json!({
                "provider_id": "org.example.dual",
                "generation": gen_b,
                "target_lang": "zh-Hans"
            }),
        )
        .await
        .unwrap();
    assert_eq!(tr_probe["ok"], true);
    assert_eq!(tr_probe["stage"], "done");
    assert_eq!(tr_probe["reason"], "ok");

    // 同时检查 listExternalPlugins 中 transport provider_registered 仍为 true，且 probeAccount 命中连接 A！
    let ext = unix_rpc(&socket)
        .call("listExternalPlugins", json!({}))
        .await
        .unwrap();
    assert_eq!(ext["plugins"][0]["provider_registered"], true);

    let account_probe = unix_rpc(&socket)
        .call(
            "probeAccount",
            json!({
                "draft": {
                    "display_name": "Dual",
                    "address": "dual@example.test",
                    "incoming": {
                        "kind": "plugin",
                        "provider": "org.example.dual",
                        "poll_secs": 3600
                    }
                },
                "credentials": {}
            }),
        )
        .await
        .unwrap();
    assert_eq!(account_probe["incoming"]["ok"], true);

    transport_task.abort();
    translation_task.abort();
    stop_daemon(daemon, shutdown).await;
}

#[tokio::test]
async fn full_lifecycle_install_enable_disable_independent_upgrade_rollback_remove_and_builtin_guard() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(dir.path());
    write_daemon_config(&paths, "");
    install_frontend_package(
        &paths,
        maild_brand::BUNDLED_FRONTEND_ID,
        "\"plugins_manage\"",
    );

    let (mut daemon, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket(&mut daemon, &socket).await;

    let mut studio = StudioStreamClient::from_conn(
        connect_authenticated(&socket, "frontend", maild_brand::BUNDLED_FRONTEND_ID).await,
    );

    // 0) builtin.* manifest ID 必须被安装事务拒绝 (-32017)
    let builtin_src = dir.path().join("src-builtin");
    chmod_dir(&builtin_src);
    let builtin_pkg = install_external_v2_package(
        &builtin_src,
        "builtin.openai_compatible",
        "1.0.0",
        "\"translation_provider\"",
    );
    let staged_builtin = stage_plugin_package_into(&builtin_pkg, &paths.plugin_dir).unwrap();
    let builtin_err = unix_rpc(&socket)
        .call(
            "commitPluginInstall",
            json!({ "staging": staged_builtin.staging() }),
        )
        .await
        .unwrap_err();
    assert_eq!(builtin_err.rpc_code, Some(maild::api::PLUGIN_TRANSACTION_FAILED));

    // 1) 安装 v1.0.0 (independent)
    let src_v1 = dir.path().join("src-v1");
    chmod_dir(&src_v1);
    let pkg_v1 = install_external_v2_package(
        &src_v1,
        "org.example.life",
        "1.0.0",
        "\"translation_provider\"",
    );
    let staged_v1 = stage_plugin_package_into(&pkg_v1, &paths.plugin_dir).unwrap();
    unix_rpc(&socket)
        .call("commitPluginInstall", json!({ "staging": staged_v1.staging() }))
        .await
        .unwrap();

    let push1 = studio.next_providers_push().await;
    assert_eq!(push1.providers.len(), 1);
    assert_eq!(push1.providers[0].id, "org.example.life");
    assert!(!push1.providers[0].enabled);
    assert!(!push1.providers[0].registered);
    assert_eq!(push1.providers[0].version, "1.0.0");

    // 未启用时拒绝注册 (-32016)
    let mut disabled_plugin = connect_authenticated(&socket, "plugin", "org.example.life").await;
    let reg_disabled = disabled_plugin
        .call("translationProviderRegister", json!({}), |_| {})
        .await
        .unwrap_err();
    assert_eq!(reg_disabled.rpc_code, Some(maild::api::METHOD_NOT_PERMITTED));

    // 2) 启用并注册 v1.0.0
    unix_rpc(&socket)
        .call(
            "setPluginEnabled",
            json!({ "id": "org.example.life", "enabled": true }),
        )
        .await
        .unwrap();
    let push_enabled = studio.next_providers_push().await;
    assert!(push_enabled.providers[0].enabled);

    let reg1 = disabled_plugin
        .call(
            "translationProviderRegister",
            json!({ "variant_revision": "v1-rev" }),
            |_| {},
        )
        .await
        .unwrap();
    let gen1 = reg1["generation"].as_u64().unwrap();
    let push_reg1 = studio.next_providers_push().await;
    assert!(push_reg1.providers[0].registered);
    assert_eq!(push_reg1.providers[0].generation, gen1);
    assert_eq!(push_reg1.providers[0].l10n["zh-Hans"].name.as_deref(), Some("测试翻译器"));

    // 3) 停用 independent 插件（socket 仍保持打开！）→ 必须立即撤销 gen1！
    unix_rpc(&socket)
        .call(
            "setPluginEnabled",
            json!({ "id": "org.example.life", "enabled": false }),
        )
        .await
        .unwrap();
    let push_disabled = studio.next_providers_push().await;
    assert!(!push_disabled.providers[0].enabled);
    assert!(!push_disabled.providers[0].registered);
    assert_eq!(push_disabled.providers[0].generation, 0);

    let invoke_after_disable = studio
        .call(
            "invokeTranslation",
            json!({
                "provider_id": "org.example.life",
                "generation": gen1,
                "job_id": "job-after-disable",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "items": [{ "id": 1, "text": "hi" }]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(invoke_after_disable.code, PROVIDER_UNAVAILABLE_CODE);

    // 4) 重新启用并注册 → 升级到 v1.1.0 → 必须立即撤销 gen2！
    unix_rpc(&socket)
        .call(
            "setPluginEnabled",
            json!({ "id": "org.example.life", "enabled": true }),
        )
        .await
        .unwrap();
    let _ = studio.next_providers_push().await;
    let reg2 = disabled_plugin
        .call("translationProviderRegister", json!({}), |_| {})
        .await
        .unwrap();
    let gen2 = reg2["generation"].as_u64().unwrap();
    assert!(gen2 > gen1);
    let _ = studio.next_providers_push().await;

    let src_v2 = dir.path().join("src-v2");
    chmod_dir(&src_v2);
    let pkg_v2 = install_external_v2_package(
        &src_v2,
        "org.example.life",
        "1.1.0",
        "\"translation_provider\"",
    );
    let staged_v2 = stage_plugin_package_into(&pkg_v2, &paths.plugin_dir).unwrap();
    unix_rpc(&socket)
        .call("commitPluginInstall", json!({ "staging": staged_v2.staging() }))
        .await
        .unwrap();
    let push_upgraded = studio.next_providers_push().await;
    assert_eq!(push_upgraded.providers[0].version, "1.1.0");
    assert!(!push_upgraded.providers[0].registered);
    assert_eq!(push_upgraded.providers[0].generation, 0);

    let invoke_after_upgrade = studio
        .call(
            "invokeTranslation",
            json!({
                "provider_id": "org.example.life",
                "generation": gen2,
                "job_id": "job-after-upgrade",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "items": [{ "id": 1, "text": "hi" }]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(invoke_after_upgrade.code, PROVIDER_UNAVAILABLE_CODE);

    // 5) 重新注册 v1.1.0 → 回滚到 v1.0.0 → 必须立即撤销 gen3！
    let reg3 = disabled_plugin
        .call("translationProviderRegister", json!({}), |_| {})
        .await
        .unwrap();
    let gen3 = reg3["generation"].as_u64().unwrap();
    assert!(gen3 > gen2);
    let _ = studio.next_providers_push().await;

    unix_rpc(&socket)
        .call("rollbackPlugin", json!({ "id": "org.example.life" }))
        .await
        .unwrap();
    let push_rollback = studio.next_providers_push().await;
    assert_eq!(push_rollback.providers[0].version, "1.0.0");
    assert!(!push_rollback.providers[0].registered);

    let invoke_after_rollback = studio
        .call(
            "invokeTranslation",
            json!({
                "provider_id": "org.example.life",
                "generation": gen3,
                "job_id": "job-after-rollback",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "items": [{ "id": 1, "text": "hi" }]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(invoke_after_rollback.code, PROVIDER_UNAVAILABLE_CODE);

    // 6) 移除插件 → 撤销并从 listTranslationProviders 消失
    let reg4 = disabled_plugin
        .call("translationProviderRegister", json!({}), |_| {})
        .await
        .unwrap();
    let gen4 = reg4["generation"].as_u64().unwrap();
    assert!(gen4 > gen3);
    let _ = studio.next_providers_push().await;

    unix_rpc(&socket)
        .call("removePlugin", json!({ "id": "org.example.life" }))
        .await
        .unwrap();
    let listed = studio.call("listTranslationProviders", json!({})).await.unwrap();
    assert!(listed["providers"].as_array().unwrap().is_empty());

    drop(disabled_plugin);
    stop_daemon(daemon, shutdown).await;
}

#[tokio::test]
async fn principal_and_origin_privilege_boundaries_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(dir.path());
    write_daemon_config(&paths, "");
    install_frontend_package(
        &paths,
        maild_brand::BUNDLED_FRONTEND_ID,
        "\"plugins_manage\"",
    );
    // 第三方前端（即使拥有 inbox/message/settings 路由，也不是受托 Studio，且无 plugins_manage）
    install_frontend_package(&paths, "org.example.third-party-ui", "");
    install_external_v2_package(
        &paths.plugin_dir,
        "org.example.tr",
        "1.0.0",
        "\"translation_provider\", \"messages_read\", \"settings_read\"",
    );

    let (mut daemon, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket(&mut daemon, &socket).await;

    unix_rpc(&socket)
        .call(
            "setPluginEnabled",
            json!({ "id": "org.example.tr", "enabled": true }),
        )
        .await
        .unwrap();

    // 1) 未认证 User（Unix socket）不能调 invoke/cancel/test/register
    let user = unix_rpc(&socket);
    let invoke_params = json!({
        "provider_id": "org.example.tr",
        "generation": 1,
        "job_id": "job-1",
        "source_lang": "en",
        "target_lang": "zh-Hans",
        "content_kind": "plain",
        "items": [{ "id": 1, "text": "hello" }]
    });
    assert_eq!(
        user.call("invokeTranslation", invoke_params.clone())
            .await
            .unwrap_err()
            .rpc_code,
        Some(maild::api::METHOD_NOT_PERMITTED)
    );
    assert_eq!(
        user.call("cancelTranslation", json!({ "job_id": "job-1" }))
            .await
            .unwrap_err()
            .rpc_code,
        Some(maild::api::METHOD_NOT_PERMITTED)
    );
    assert_eq!(
        user.call(
            "testTranslationProvider",
            json!({ "provider_id": "org.example.tr", "target_lang": "zh-Hans" })
        )
        .await
        .unwrap_err()
        .rpc_code,
        Some(maild::api::METHOD_NOT_PERMITTED)
    );
    assert_eq!(
        user.call("translationProviderRegister", json!({}))
            .await
            .unwrap_err()
            .rpc_code,
        Some(maild::api::INVALID_REQUEST)
    );

    // 2) 非 Studio 第三方前端被拒绝 invoke/cancel/test/listTranslationProviders
    let mut other_ui = connect_authenticated(&socket, "frontend", "org.example.third-party-ui").await;
    for (method, params) in [
        ("invokeTranslation", invoke_params.clone()),
        ("cancelTranslation", json!({ "job_id": "job-1" })),
        (
            "testTranslationProvider",
            json!({ "provider_id": "org.example.tr", "target_lang": "zh-Hans" }),
        ),
        ("listTranslationProviders", json!({})),
    ] {
        let err: ClientError = other_ui.call(method, params, |_| {}).await.unwrap_err();
        assert_eq!(
            err.rpc_code,
            Some(maild::api::METHOD_NOT_PERMITTED),
            "third-party frontend must be denied on {method}"
        );
    }

    // 3) 即使声明了 messages_read + settings_read 的插件主体也不能调 invoke/cancel/test/list，
    //    且不会收到 translation/providers 推送！
    let mut plugin = connect_authenticated(&socket, "plugin", "org.example.tr").await;
    let mut saw_push_on_plugin = false;
    for (method, params) in [
        ("invokeTranslation", invoke_params.clone()),
        ("cancelTranslation", json!({ "job_id": "job-1" })),
        (
            "testTranslationProvider",
            json!({ "provider_id": "org.example.tr", "target_lang": "zh-Hans" }),
        ),
        ("listTranslationProviders", json!({})),
    ] {
        let err = plugin
            .call(method, params, |n| {
                if n.method == TRANSLATION_PROVIDERS_NOTIFICATION {
                    saw_push_on_plugin = true;
                }
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.rpc_code,
            Some(maild::api::METHOD_NOT_PERMITTED),
            "plugin principal must be denied on {method}"
        );
    }
    // 注册触发 translation/providers 广播；无 plugins_manage 的 plugin/other_ui 都不应收到！
    plugin
        .call("translationProviderRegister", json!({}), |n| {
            if n.method == TRANSLATION_PROVIDERS_NOTIFICATION {
                saw_push_on_plugin = true;
            }
        })
        .await
        .unwrap();
    assert!(!saw_push_on_plugin);
    let (mut other_reader, _other_writer) = other_ui.into_split();
    let other_push =
        tokio::time::timeout(Duration::from_millis(120), other_reader.next_incoming()).await;
    assert!(
        other_push.is_err(),
        "无 plugins_manage 权限的第三方前端不得收到 translation/providers 推送"
    );

    // 4) HTTP 来源的 User（即使已带 Bearer）也不得绕过 invoke/cancel/test
    let ctx = cordis::Context::new();
    ctx.provide(maild::translation_provider::TranslationProviderRegistry::new());
    for (method, params) in [
        ("invokeTranslation", invoke_params),
        ("cancelTranslation", json!({ "job_id": "job-1" })),
        (
            "testTranslationProvider",
            json!({ "provider_id": "org.example.tr", "target_lang": "zh-Hans" }),
        ),
    ] {
        let req = maild::api::JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
            id: json!(1),
        };
        let resp = maild::api::dispatch_with_origin(&ctx, maild::api::RequestOrigin::Http, &req).await;
        assert_eq!(
            resp.error.as_ref().map(|e| e.code),
            Some(maild::api::METHOD_NOT_PERMITTED),
            "HTTP User must be rejected on {method}"
        );
    }

    stop_daemon(daemon, shutdown).await;
}
