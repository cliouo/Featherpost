//! M4-05 反向调用预算与错误码映射（ADR-0021 §5）。
//!
//! 独立测试进程：`MAILD_TEST_PROVIDER_TIMEOUT_MS` 是进程级 hermetic 缝
//! （OnceLock 缓存），必须在任何预算消费前设置，因此超时/上游错误用例
//! 与主 e2e 分文件隔离。覆盖：慢 provider → `-32019`；插件上报业务失败 →
//! `-32020` 且 detail 有界脱敏。

use std::sync::Mutex as StdMutex;
use std::time::Duration;

use maild::{run_daemon_until, DaemonOptions};
use maild_client::{ClientError, Endpoint, Incoming, JsonRpcError, RpcClient};
use maild_paths::AppPaths;
use serde_json::{json, Value};

fn use_memory_credential_store() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
}

const PROVIDER_ID: &str = "org.example.slow";

fn test_paths(root: &std::path::Path) -> AppPaths {
    AppPaths::from_roots(
        &root.join("config"),
        &root.join("data"),
        Some(&root.join("runtime")),
        Some(&root.join("state")),
        root,
    )
}

#[derive(Default)]
struct SlowScript {
    delay: StdMutex<Option<Duration>>,
    /// 第 N 次 transport/send 以该 error 应答（按到达序计数；迟到请求也算数，
    /// 避免超时用例的迟到请求吞掉错误注入）。
    fail_send_ordinal: StdMutex<Option<(u32, JsonRpcError)>>,
    send_count: std::sync::atomic::AtomicU32,
}

async fn run_provider(
    socket: &std::path::Path,
    token: &str,
    script: std::sync::Arc<SlowScript>,
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
            let incoming = match reader.next_incoming().await {
                Ok(incoming) => incoming,
                Err(error) => {
                    eprintln!("[fixture/provider] 读循环退出: {error}");
                    break;
                }
            };
            let Incoming::Request { id, method, params } = incoming else {
                eprintln!("[fixture/provider] 收到非请求帧，忽略");
                continue;
            };
            let delay = *script.delay.lock().unwrap();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let outcome: Result<Value, JsonRpcError> = match method.as_str() {
                "transport/probe" => Ok(json!({ "ok": true, "stage": "provider" })),
                "transport/send" => {
                    let ordinal = script
                        .send_count
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        + 1;
                    let injected = script.fail_send_ordinal.lock().unwrap().take();
                    match injected {
                        // 注入目标必须是当前这次发送；否则原样放回。
                        Some((target, error)) if target == ordinal => Err(error),
                        other => {
                            *script.fail_send_ordinal.lock().unwrap() = other;
                            Ok(json!({}))
                        }
                    }
                }
                "transport/receive" => Ok(json!({ "messages": [] })),
                _ => {
                    let _ = params;
                    Ok(json!({}))
                }
            };
            if writer.respond(&id, outcome).await.is_err() {
                break;
            }
        }
    });
    Ok(task)
}

#[tokio::test(flavor = "current_thread")]
async fn slow_provider_times_out_and_upstream_error_maps_to_32020() {
    // 预算覆盖缝：300ms。必须在任何预算消费（首个反向调用）前设置——
    // 本文件是独立进程，OnceLock 首读发生在本测试内。
    std::env::set_var("MAILD_TEST_PROVIDER_TIMEOUT_MS", "300");
    use_memory_credential_store();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    let package = paths.plugin_dir.join(PROVIDER_ID);
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("plugin.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{PROVIDER_ID}\"\nname = \"Slow fixture\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"transport_provider\"]\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for dir in [
            &paths.config_dir,
            &paths.data_dir,
            &paths.runtime_dir,
            &paths.plugin_dir,
            &paths.frontend_dir,
        ] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    std::fs::write(
        &paths.config_file,
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

    let options =
        DaemonOptions::new(paths.config_file.clone(), paths.clone()).expect("绝对路径");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let daemon = tokio::spawn(run_daemon_until(options, async move {
        let _ = shutdown_rx.await;
    }));
    let socket = paths.runtime_dir.join("mail.sock");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "socket 未在期限内出现"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let client = RpcClient::new(Endpoint::Unix {
        path: socket.to_path_buf(),
    })
    .unwrap();
    let token = client.reveal_plugin_token(PROVIDER_ID, None).await.unwrap();
    let script = std::sync::Arc::new(SlowScript::default());
    let provider = run_provider(&socket, &token, script.clone()).await.unwrap();

    // probe 快速应答 → 创建委托账户成功。
    let created = client
        .call(
            "createAccount",
            json!({
                "draft": {
                    "display_name": "慢中继",
                    "address": "me@example.test",
                    "incoming": {"kind": "plugin", "provider": PROVIDER_ID, "poll_secs": 3600},
                    "outgoing": {"kind": "plugin", "provider": PROVIDER_ID}
                },
                "credentials": {}
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("createAccount 失败: {error}"));
    let account_id = created["profile"]["id"].as_str().unwrap().to_owned();

    // 慢应答超过覆盖预算（300ms）→ -32019 PROVIDER_TIMEOUT。
    *script.delay.lock().unwrap() = Some(Duration::from_millis(900));
    let error = client
        .call(
            "sendFromAccount",
            json!({ "account": account_id, "to": "friend@example.test", "subject": "慢", "body": "超时" }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32019), "超预算必须 -32019");

    // 恢复速度但以业务 error 应答下一次发送 → -32020 PROVIDER_ERROR，detail 有界脱敏。
    *script.delay.lock().unwrap() = None;
    // 等 provider 处理完迟到的第 1 次发送（串行应答循环），避免排队吃掉预算。
    let idle_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while script.send_count.load(std::sync::atomic::Ordering::SeqCst) < 1 {
        assert!(
            tokio::time::Instant::now() < idle_deadline,
            "provider 未在期限内处理完第一次发送"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let long_message = "x".repeat(500);
    // 第 2 次发送（第 1 次已因超时结束；其迟到请求不占用注入序号）。
    *script.fail_send_ordinal.lock().unwrap() = Some((
        2,
        JsonRpcError {
            code: -32099,
            message: format!("上游拒绝 {long_message}\u{7}"),
            data: None,
        },
    ));
    let error = client
        .call(
            "sendFromAccount",
            json!({ "account": account_id, "to": "friend@example.test", "subject": "拒", "body": "上游失败" }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32020), "插件业务失败必须 -32020");
    assert!(error.message.contains("上游拒绝"));
    assert!(error.message.chars().all(|ch| !ch.is_control()), "detail 必须脱敏控制字符");
    assert!(
        error.message.chars().count() < 400,
        "detail 必须有界: {}",
        error.message.chars().count()
    );

    provider.abort();
    let _ = shutdown_tx.send(());
    daemon.await.unwrap().unwrap();
}
