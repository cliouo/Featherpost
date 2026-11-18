//! M4-05 transport provider 验收（ADR-0021 实施切片）。
//!
//! 覆盖：providerRegister 的主体与 capability 门控（User 拒绝、无词表 -32016）、
//! 委托账户的 probe→create→send 全链路落到 provider、委托收信经既有
//! store/filters waterfall 落库且游标推进不重复投递、provider 断线后 send
//! 快速失败 -32018 且内核其余能力照常。测试进程自身充当 provider（independent
//! 包 + reveal 凭证 + StreamConnection 反向应答），完全 hermetic。
//! 超时/上游错误码映射在独立进程文件 `plugin_transport_timeout.rs`（预算覆盖缝
//! 是进程级 env，避免并行测试串扰）。

use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use maild::{run_daemon_until, DaemonOptions};
use maild_client::{ClientError, Endpoint, Incoming, JsonRpcError, RpcClient};
use maild_paths::AppPaths;
use serde_json::{json, Value};

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

/// 在受管 plugins 目录写入一个 independent fixture 包（不设执行位需求）。
fn install_independent_package(paths: &AppPaths, id: &str, capabilities: &str) {
    let package = paths.plugin_dir.join(id);
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("plugin.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Transport fixture\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [{capabilities}]\n"
        ),
    )
    .unwrap();
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

fn rpc_code(error: &ClientError) -> Option<i64> {
    error.rpc_code
}

// —— 测试内 provider 脚本 ——

#[derive(Default)]
struct ProviderScript {
    /// transport/probe 是否回答 ok=true。
    probe_ok: bool,
    /// transport/probe 收到的参数日志。
    probe_log: StdMutex<Vec<Value>>,
    /// transport/send 收到的参数日志。
    send_log: StdMutex<Vec<Value>>,
    /// transport/receive 收到的参数日志。
    receive_log: StdMutex<Vec<Value>>,
    /// transport/receive 的应答队列；空队列按 `{"messages":[]}` 应答。
    receive_batches: StdMutex<VecDeque<Value>>,
    /// 应答前的固定延迟（超时用例由独立进程文件覆盖预算后使用）。
    delay: StdMutex<Option<Duration>>,
    /// 非 None 时以该 error 对象应答全部反向调用。
    reply_error: StdMutex<Option<JsonRpcError>>,
}

impl ProviderScript {
    fn new(probe_ok: bool) -> Arc<Self> {
        Arc::new(Self {
            probe_ok,
            ..Default::default()
        })
    }

    fn push_batch(&self, messages: Value) {
        self.receive_batches
            .lock()
            .unwrap()
            .push_back(json!({ "messages": messages }));
    }

    fn sends(&self) -> Vec<Value> {
        self.send_log.lock().unwrap().clone()
    }

    fn probes(&self) -> Vec<Value> {
        self.probe_log.lock().unwrap().clone()
    }

    fn receives(&self) -> Vec<Value> {
        self.receive_log.lock().unwrap().clone()
    }
}

use std::sync::Arc;

/// 充当 provider：认证 → 注册 → 循环应答反向请求。返回任务句柄用于断连。
async fn run_provider(
    socket: &std::path::Path,
    token: &str,
    script: Arc<ProviderScript>,
) -> Result<tokio::task::JoinHandle<()>, ClientError> {
    let mut conn = RpcClient::new(Endpoint::Unix {
        path: socket.to_path_buf(),
    })?
    .connect_stream()
    .await?;
    conn.authenticate(token).await?;
    conn.call("providerRegister", json!({}), |_| {}).await?;
    let (mut reader, mut writer) = conn.into_split();
    let task = tokio::spawn(async move {
        loop {
            let Ok(Incoming::Request { id, method, params }) = reader.next_incoming().await else {
                break;
            };
            let delay = *script.delay.lock().unwrap();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let outcome: Result<Value, JsonRpcError> = match method.as_str() {
                "transport/probe" => {
                    script.probe_log.lock().unwrap().push(params);
                    Ok(json!({
                        "ok": script.probe_ok,
                        "stage": "provider",
                        "detail": if script.probe_ok { "" } else { "fixture 拒绝" },
                    }))
                }
                "transport/send" => {
                    script.send_log.lock().unwrap().push(params);
                    match script.reply_error.lock().unwrap().take() {
                        Some(error) => Err(error),
                        None => Ok(json!({})),
                    }
                }
                "transport/receive" => {
                    script.receive_log.lock().unwrap().push(params);
                    Ok(script
                        .receive_batches
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or_else(|| json!({ "messages": [] })))
                }
                _ => Err(JsonRpcError {
                    code: -32601,
                    message: format!("fixture 不认识 {method}"),
                    data: None,
                }),
            };
            if writer.respond(&id, outcome).await.is_err() {
                break;
            }
        }
    });
    Ok(task)
}

const PROVIDER_ID: &str = "org.example.transport";
const SECOND_PROVIDER_ID: &str = "org.example.transport-second";
const NO_CAP_ID: &str = "org.example.nocap";

fn plugin_account_params(address: &str, incoming_provider: &str, outgoing_provider: &str) -> Value {
    json!({
        "draft": {
            "display_name": format!("{incoming_provider} delegated mailbox"),
            "address": address,
            "incoming": {
                "kind": "plugin",
                "provider": incoming_provider,
                "poll_secs": 3_600
            },
            "outgoing": {"kind": "plugin", "provider": outgoing_provider}
        },
        "credentials": {}
    })
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_mailbox_binding_is_unique_per_provider_and_live_profile() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    install_independent_package(&paths, PROVIDER_ID, "\"transport_provider\"");
    install_independent_package(&paths, SECOND_PROVIDER_ID, "\"transport_provider\"");
    write_config(&paths, "[plugins.external.config]\n");

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let control = unix_client(&socket);
    let first_token = control
        .reveal_plugin_token(PROVIDER_ID, None)
        .await
        .unwrap();
    let second_token = control
        .reveal_plugin_token(SECOND_PROVIDER_ID, None)
        .await
        .unwrap();
    let first_script = ProviderScript::new(true);
    let second_script = ProviderScript::new(true);
    let first_provider = run_provider(&socket, &first_token, first_script.clone())
        .await
        .unwrap();
    let second_provider = run_provider(&socket, &second_token, second_script)
        .await
        .unwrap();

    // 两个并发 draft 在同一 mutation lock 下检查；域名大小写差异归一后只能一个成功。
    // 成功 profile 的 incoming/outgoing 是同一 pair，不能把它自己双计为冲突。
    let first_client = unix_client(&socket);
    let second_client = unix_client(&socket);
    let (left, right) = tokio::join!(
        first_client.call(
            "createAccount",
            plugin_account_params("race@EXAMPLE.test", PROVIDER_ID, PROVIDER_ID),
        ),
        second_client.call(
            "createAccount",
            plugin_account_params("race@example.TEST", PROVIDER_ID, PROVIDER_ID),
        )
    );
    let (winner, loser) = match (left, right) {
        (Ok(profile), Err(error)) | (Err(error), Ok(profile)) => (profile, error),
        outcomes => panic!("并发重复创建必须恰好一成一败: {outcomes:?}"),
    };
    let winner_id = winner["profile"]["id"].as_str().unwrap();
    assert_eq!(winner["profile"]["revision"], 1);
    assert_eq!(loser.rpc_code, Some(-32011));
    assert_eq!(loser.message, "插件 provider 与邮箱地址组合已存在");
    assert_eq!(
        first_script.probes().len(),
        2,
        "冲突 draft 必须在 provider probe 前被拒绝"
    );

    // 普通更新必须忽略正在更新的 profile 自身，否则它会与自己的 pair 冲突。
    let self_updated = control
        .call(
            "updateAccount",
            json!({
                "id": winner_id,
                "expected_revision": 1,
                "patch": {"display_name": "self update remains valid"}
            }),
        )
        .await
        .expect("唯一性检查必须忽略更新目标自身");
    assert_eq!(self_updated["revision"], 2);

    // Read-only checks for an existing provider account carry its binding identity.
    let mut probe_existing=plugin_account_params("race@example.test",PROVIDER_ID,PROVIDER_ID);
    probe_existing["id"]=json!(winner_id);probe_existing["expected_revision"]=json!(2);
    control.call("probeAccount",probe_existing.clone()).await.unwrap();
    let probes=first_script.probes();
    assert!(probes.iter().rev().take(2).all(|p| p["account"]==winner_id));
    probe_existing["expected_revision"]=json!(1);
    assert_eq!(control.call("probeAccount",probe_existing).await.unwrap_err().rpc_code,Some(-32011));
    assert_eq!(first_script.probes().len(),probes.len(),"stale revision does not call provider");

    // 同一规范地址由不同 provider 管理不冲突。
    control
        .call(
            "createAccount",
            plugin_account_params("race@example.test", SECOND_PROVIDER_ID, SECOND_PROVIDER_ID),
        )
        .await
        .expect("不同 provider 可绑定同一邮箱");

    // 只在 outgoing 方向复用既有 provider+address 也必须冲突；incoming 使用另一
    // provider，确保此断言独立覆盖 outgoing pair 提取。
    control
        .call(
            "createAccount",
            plugin_account_params("outgoing-only@example.test", PROVIDER_ID, PROVIDER_ID),
        )
        .await
        .expect("建立 outgoing 冲突基线");
    let outgoing_conflict = control
        .call(
            "createAccount",
            plugin_account_params(
                "outgoing-only@example.test",
                SECOND_PROVIDER_ID,
                PROVIDER_ID,
            ),
        )
        .await
        .expect_err("outgoing-only 重复绑定必须拒绝");
    assert_eq!(outgoing_conflict.rpc_code, Some(-32011));

    // update 重组出的 candidate 同样在发布前检查，并忽略自身旧 profile。
    let update_target = control
        .call(
            "createAccount",
            plugin_account_params("update-target@example.test", PROVIDER_ID, PROVIDER_ID),
        )
        .await
        .unwrap();
    let update_source = control
        .call(
            "createAccount",
            plugin_account_params("update-source@example.test", PROVIDER_ID, PROVIDER_ID),
        )
        .await
        .unwrap();
    let source_id = update_source["profile"]["id"].as_str().unwrap();
    let update_conflict = control
        .call(
            "updateAccount",
            json!({
                "id": source_id,
                "expected_revision": 1,
                "patch": {"address": "update-target@EXAMPLE.TEST"}
            }),
        )
        .await
        .expect_err("更新不得占用另一 live profile 的 provider+address");
    assert_eq!(update_conflict.rpc_code, Some(-32011));
    assert_eq!(
        update_conflict.message,
        "插件 provider 与邮箱地址组合已存在"
    );
    assert_eq!(update_target["profile"]["revision"], 1);
    let profiles = control
        .call("listAccountProfiles", json!({"include_archived": true}))
        .await
        .unwrap();
    let unchanged = profiles["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|profile| profile["id"] == source_id)
        .unwrap();
    assert_eq!(unchanged["revision"], 1, "冲突更新不得发布新 revision");
    assert_eq!(unchanged["address"], "update-source@example.test");

    // 归档 profile 不再占用该 pair，随后可以重新创建。
    let archived = control
        .call(
            "createAccount",
            plugin_account_params("reusable@example.test", PROVIDER_ID, PROVIDER_ID),
        )
        .await
        .unwrap();
    control
        .call(
            "removeAccount",
            json!({
                "id": archived["profile"]["id"],
                "expected_revision": archived["profile"]["revision"]
            }),
        )
        .await
        .unwrap();
    control
        .call(
            "createAccount",
            plugin_account_params("reusable@EXAMPLE.TEST", PROVIDER_ID, PROVIDER_ID),
        )
        .await
        .expect("归档 profile 不占用 provider+address pair");

    first_provider.abort();
    second_provider.abort();
    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn delegated_send_receive_and_disconnect_fail_closed() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    install_independent_package(&paths, PROVIDER_ID, "\"transport_provider\"");
    // 无词表的对照插件也要在启动扫描前就位（受管目录由 Agent 独占写，测试不做
    // 事务安装）。
    install_independent_package(&paths, NO_CAP_ID, "\"messages_read\"");
    write_config(&paths, "[plugins.external.config]\n");

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    // 1. 门控：User 主体注册被拒（INVALID_REQUEST）；无 capability 插件 -32016。
    let client = unix_client(&socket);
    assert_eq!(client.whoami().await.unwrap(), "user");
    let mut user_conn = client.connect_stream().await.unwrap();
    let error = user_conn
        .call("providerRegister", json!({}), |_| {})
        .await
        .unwrap_err();
    assert_eq!(rpc_code(&error), Some(-32600), "User 主体注册按 INVALID_REQUEST 拒绝");
    drop(user_conn);

    install_independent_package(&paths, NO_CAP_ID, "\"messages_read\"");
    let nocap_token = client.reveal_plugin_token(NO_CAP_ID, None).await.unwrap();
    let mut nocap_conn = client.connect_stream().await.unwrap();
    nocap_conn.authenticate(&nocap_token).await.unwrap();
    let error = nocap_conn
        .call("providerRegister", json!({}), |_| {})
        .await
        .unwrap_err();
    assert_eq!(rpc_code(&error), Some(-32016), "缺 transport_provider 词表必须 -32016");
    drop(nocap_conn);

    // 2. 正式 provider 上线并注册。
    let token = client.reveal_plugin_token(PROVIDER_ID, None).await.unwrap();
    let script = ProviderScript::new(true);
    script.push_batch(json!([
        {"uid": "5", "sender": "alice@example.test", "subject": "第一封", "body": "正文五", "received_at": 1_755_000_000},
        {"uid": "6", "sender": "bob@example.test", "subject": "第二封", "body": "正文六", "received_at": 1_755_000_060}
    ]));
    let provider_task = run_provider(&socket, &token, script.clone()).await.unwrap();

    // 3. 双向委托的账户：probe 走 provider → 创建成功且无凭据引用。
    let created = client
        .call(
            "createAccount",
            json!({
                "draft": {
                    "display_name": "委托邮箱",
                    "address": "me@example.test",
                    "incoming": {"kind": "plugin", "provider": PROVIDER_ID, "poll_secs": 1},
                    "outgoing": {"kind": "plugin", "provider": PROVIDER_ID}
                },
                "credentials": {}
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("createAccount 失败: {error}"));
    let profile = &created["profile"];
    let account_id = profile["id"].as_str().unwrap().to_owned();
    assert_eq!(profile["incoming"]["kind"], "plugin");
    assert_eq!(profile["incoming"]["provider"], PROVIDER_ID);
    assert_eq!(profile["credentials"]["incoming"], "missing");
    let probes = script.probes();
    assert_eq!(probes.len(), 2, "双向委托必须分别探测 incoming/outgoing");
    assert!(probes.iter().all(|params| params["account"] == ""));
    assert!(
        probes
            .iter()
            .all(|params| params["address"] == "me@example.test"),
        "draft 探测必须携带规范邮箱地址"
    );

    // 4. 委托发信落到 provider，载荷字段由内核填充。
    client
        .call(
            "sendFromAccount",
            json!({
                "account": account_id,
                "to": "friend@example.test",
                "subject": "经中继",
                "body": "你好"
            }),
        )
        .await
        .unwrap();
    let sends = script.sends();
    assert_eq!(sends.len(), 1, "sendFromAccount 必须恰好触发一次反向调用");
    assert_eq!(sends[0]["account"], account_id.as_str());
    assert_eq!(sends[0]["account_address"], "me@example.test");
    assert_eq!(sends[0]["from"], "me@example.test");
    assert_eq!(sends[0]["to"], "friend@example.test");

    // account_address 始终是规范远端邮箱；alias 只出现在 from，API provider
    // 因而可以明确接受或拒绝别名，不必从 from 猜 mailbox。
    client
        .call(
            "createAddress",
            json!({
                "account": account_id,
                "kind": "alias",
                "address": "alias@example.test"
            }),
        )
        .await
        .unwrap();
    client
        .call(
            "sendFromAccount",
            json!({
                "account": account_id,
                "from_address": "alias@example.test",
                "to": "friend@example.test",
                "subject": "别名",
                "body": "仍由同一远端邮箱发送"
            }),
        )
        .await
        .unwrap();
    let sends = script.sends();
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[1]["account_address"], "me@example.test");
    assert_eq!(sends[1]["from"], "alias@example.test");

    // 5. 委托收信进入既有落库链路；重复 uid 幂等，新 uid 追加。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let messages = client
            .call(
                "listMessageSummaries",
                json!({ "account": account_id, "limit": 50 }),
            )
            .await
            .unwrap();
        let count = messages["messages"].as_array().map(Vec::len).unwrap_or(0);
        if count >= 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "委托收信未在期限内落库: {messages}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        script
            .receives()
            .iter()
            .all(|params| params["address"] == "me@example.test"),
        "委托收信必须携带规范邮箱地址"
    );
    // 下一轮询 tick：uid=5 重复（应幂等跳过）、uid=7 新增。
    script.push_batch(json!([
        {"uid": "5", "sender": "alice@example.test", "subject": "第一封", "body": "正文五", "received_at": 1_755_000_000},
        {"uid": "7", "sender": "carol@example.test", "subject": "第三封", "body": "正文七", "received_at": 1_755_000_120}
    ]));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let messages = client
            .call(
                "listMessageSummaries",
                json!({ "account": account_id, "limit": 50 }),
            )
            .await
            .unwrap();
        let items = messages["messages"].as_array().unwrap();
        let uids: Vec<&str> = items.iter().filter_map(|m| m["uid"].as_str()).collect();
        if uids.contains(&"7") {
            assert_eq!(uids.len(), 3, "重复 uid 不得重复投递: {uids:?}");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "第二批委托收信未在期限内落库: {messages}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 6. provider 账户只改 address 时必须重新 probe 并重挂收件 runtime，避免
    // 目录、发件参数与后台轮询各自持有不同地址。
    let updated = client
        .call(
            "updateAccount",
            json!({
                "id": account_id,
                "expected_revision": 1,
                "patch": {"address": "moved@example.test"}
            }),
        )
        .await
        .unwrap();
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["address"], "moved@example.test");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let probed = script
            .probes()
            .iter()
            .any(|params| {
                params["account"] == account_id
                    && params["address"] == "moved@example.test"
            });
        let received = script
            .receives()
            .iter()
            .any(|params| params["address"] == "moved@example.test");
        if probed && received {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "address 更新后 provider 未按新地址 probe 并重挂收件 runtime"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    client
        .call(
            "sendFromAccount",
            json!({
                "account": account_id,
                "to": "friend@example.test",
                "subject": "地址更新后",
                "body": "使用新规范地址"
            }),
        )
        .await
        .unwrap();
    let sends = script.sends();
    assert_eq!(sends.len(), 3);
    assert_eq!(sends[2]["account_address"], "moved@example.test");
    assert_eq!(sends[2]["from"], "moved@example.test");

    // 7. provider 断线 → 注销 → sendFromAccount 立即 -32018，绝不回退内置 SMTP。
    provider_task.abort();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let error = client
            .call(
                "sendFromAccount",
                json!({
                    "account": account_id,
                    "to": "friend@example.test",
                    "subject": "断线后",
                    "body": "应当失败"
                }),
            )
            .await
            .err();
        match error {
            Some(error) => {
                assert_eq!(
                    rpc_code(&error),
                    Some(-32018),
                    "断线后必须快速失败 PROVIDER_UNAVAILABLE"
                );
                break;
            }
            None => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "provider 断线后 sendFromAccount 仍成功——发生了静默回退"
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 8. 内核其余能力照常：status 与 listExternalPlugins 观测位。
    assert!(client.call("status", json!({})).await.unwrap().is_object());
    let listing = client
        .call("listExternalPlugins", json!({}))
        .await
        .unwrap();
    let transport = listing["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["id"] == PROVIDER_ID)
        .unwrap();
    assert_eq!(
        transport["provider_registered"], false,
        "断线后 provider_registered 必须回落"
    );
    assert_eq!(transport["has_identity"], true);

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn registration_updates_observation_field() {
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    install_independent_package(&paths, PROVIDER_ID, "\"transport_provider\"");
    write_config(&paths, "[plugins.external.config]\n");

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket, 10).await;

    let client = unix_client(&socket);
    let registered_flag = || async {
        let listing = client.call("listExternalPlugins", json!({})).await.unwrap();
        listing["plugins"].as_array().unwrap()[0]["provider_registered"]
            .as_bool()
            .unwrap()
    };
    assert!(!registered_flag().await, "未注册时观测位为 false");

    let token = client.reveal_plugin_token(PROVIDER_ID, None).await.unwrap();
    let script = ProviderScript::new(true);
    let provider_task = run_provider(&socket, &token, script).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !registered_flag().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "注册后 provider_registered 未变 true"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    provider_task.abort();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while registered_flag().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "断线后 provider_registered 未回落 false"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}
