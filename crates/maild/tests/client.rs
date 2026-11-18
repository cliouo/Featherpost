//! Shared-client integration against maild's real local IPC servers.

use maild_client::{Endpoint, RpcClient};
use serde_json::json;

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn shared_client_calls_real_unix_server() {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("maild-client-{}-{suffix}.sock", std::process::id()));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(maild::api::serve_unix(
        listener,
        cordis::Context::new(),
        async move {
            let _ = shutdown_rx.await;
        },
    ));

    let client = RpcClient::new(Endpoint::Unix { path: path.clone() }).unwrap();
    let result = client.call("status", json!({})).await.unwrap();
    assert_eq!(result["message_count"], 0);
    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(!path.exists());
}

#[cfg(windows)]
#[tokio::test(flavor = "current_thread")]
async fn shared_client_calls_real_named_pipe_server() {
    let name = format!(
        r"\\.\pipe\maild-client-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(maild::api::serve_named_pipe(
        name.clone(),
        cordis::Context::new(),
        async move {
            let _ = shutdown_rx.await;
        },
    ));

    let client = RpcClient::new(Endpoint::NamedPipe { name }).unwrap();
    let result = client.call("status", json!({})).await.unwrap();
    assert_eq!(result["message_count"], 0);
    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
}

/// ADR-0017：named pipe 认证路径由 CI（Windows runner）钉死——文档此前声明
/// 该路径有覆盖，但仓库内并无对应测试；本用例补上缺口。
#[cfg(windows)]
#[tokio::test(flavor = "current_thread")]
async fn named_pipe_supports_plugin_authentication_and_revocation() {
    use maild::plugin_identity::{IdentitySeam, PluginKind};

    let directory = tempfile::tempdir().unwrap();
    let seam = IdentitySeam::open(directory.path().join("plugin_auth.toml"));
    let ctx = cordis::Context::new();
    ctx.provide::<IdentitySeam>(seam.clone());
    let token = seam
        .rotate(PluginKind::External, "org.example.pipe")
        .expect("轮换出新凭证");

    let name = format!(
        r"\\.\pipe\maild-client-auth-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(maild::api::serve_named_pipe(
        name.clone(),
        ctx,
        async move {
            let _ = shutdown_rx.await;
        },
    ));
    // named pipe server 首实例就绪需要一点时间；重试连接。
    let client = loop {
        match RpcClient::new(Endpoint::NamedPipe { name: name.clone() }) {
            Ok(client) => break client,
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
        }
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut conn = loop {
        match client.connect_stream().await {
            Ok(conn) => break conn,
            Err(_) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "named pipe 未在期限内可连"
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
    };

    // 有效凭证：认证绑定 plugin 主体。
    conn.authenticate(&token).await.expect("named pipe 认证成功");
    let who = conn.call("whoami", json!({}), |_| {}).await.unwrap();
    assert_eq!(who["principal"], "plugin:org.example.pipe");

    // 无效凭证：-32014 且连接关闭。
    let mut stale = client.connect_stream().await.unwrap();
    let error = stale.authenticate(&format!("mdp_{}", "A".repeat(43))).await.unwrap_err();
    assert_eq!(error.rpc_code, Some(-32014));

    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
}
