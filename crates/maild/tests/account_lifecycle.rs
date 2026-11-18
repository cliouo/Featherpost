//! M3.7-01 headless 账户闭环验收（ADR-0015 实施切片一）。
//!
//! 全程不编辑 TOML：通过本地 Unix socket API 动态添加账户，验证
//! 「添加 → 不重启即在线 → 收信落库 → 从同一账户发信 → 重启后继续收信 →
//! disable/enable → remove（软归档、凭据删除、历史邮件保留）」。
//!
//! hermetic：假邮件服务器（IMAP 隐式 TLS + SMTP STARTTLS，自签证书）+
//! 内存凭据 store。两个测试缝都只在 debug 构建生效（见 credential.rs）：
//! `MAILD_TEST_CREDENTIAL_STORE=memory`、`MAILD_TEST_ACCEPT_INVALID_CERTS=1`。

mod common;

use std::time::Duration;

use common::fake_mail::FakeMail;
use maild::{run_daemon_until, DaemonOptions};
use maild_client::{
    AccountDraftInput, ClientError, Endpoint, ErrorKind, IncomingInput, OutgoingInput, RpcClient,
};
use mailparse::MailHeaderMap as _;
use maild_paths::AppPaths;
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

const IMAP_PASSWORD: &str = "imap-pass-2468";
const OTP_SUBJECT: &str = "登录验证码";

fn test_paths(root: &std::path::Path) -> AppPaths {
    AppPaths::from_roots(
        &root.join("config"),
        &root.join("data"),
        Some(&root.join("runtime")),
        Some(&root.join("state")),
        root,
    )
}

/// 平台默认配置（config_path == app_paths.config_file）→ 受管账户写入口开放。
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

async fn spawn_daemon(
    paths: &AppPaths,
) -> (
    tokio::task::JoinHandle<Result<(), maild::DaemonError>>,
    tokio::sync::oneshot::Sender<()>,
    std::path::PathBuf,
) {
    let options =
        DaemonOptions::new(write_platform_config(paths), paths.clone()).expect("绝对路径");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run_daemon_until(options, async move {
        let _ = shutdown_rx.await;
    }));
    let socket = paths.runtime_dir.join("mail.sock");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "API socket 未在期限内出现"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (task, shutdown_tx, socket)
}

fn client_for(socket: &std::path::Path) -> RpcClient {
    RpcClient::new(Endpoint::Unix {
        path: socket.to_path_buf(),
    })
    .unwrap()
}

fn draft_for(server: &FakeMail) -> AccountDraftInput {
    AccountDraftInput {
        display_name: "E2E 邮箱".into(),
        address: "rcpt@test.local".into(),
        incoming: IncomingInput::imap(
            "127.0.0.1",
            server.imap_addr.port(),
            "implicit",
            "e2e-user",
            "INBOX",
            true,
            60,
        ),
        outgoing: Some(OutgoingInput::smtp(
            "127.0.0.1",
            server.smtp_addr.port(),
            "starttls",
            "e2e-user",
            // 复用收件密码：同时覆盖 shared 凭据路径。
            "incoming",
        )),
    }
}

async fn wait_online(client: &RpcClient, id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "账户 {id} 未在期限内上线"
        );
        let profiles = client.list_account_profiles(false).await.unwrap().profiles;
        if let Some(profile) = profiles.iter().find(|profile| profile.id == id) {
            match profile.runtime.state.as_str() {
                "online" => return,
                state => {
                    if !matches!(state, "connecting" | "reconnecting") {
                        panic!("账户 {id} 进入异常状态: {state} ({})", profile.runtime.detail);
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 最小 SMTP 客户端（STARTTLS）：向假服务器投递一封含验证码的邮件。
/// 协商阶段逐字节读行，避免缓冲预取吞掉 TLS ClientHello。
async fn deliver_mail(addr: std::net::SocketAddr, code: &str) {
    async fn read_line_exact<S>(stream: &mut S) -> String
    where
        S: tokio::io::AsyncRead + Unpin,
    {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).await.expect("读响应");
            assert!(n > 0, "连接提前关闭");
            if byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        String::from_utf8_lossy(&line)
            .trim_end_matches('\r')
            .to_string()
    }

    async fn read_reply<S>(stream: &mut S) -> String
    where
        S: tokio::io::AsyncRead + Unpin,
    {
        loop {
            let line = read_line_exact(stream).await;
            if line.len() >= 4 && line.as_bytes()[3] == b' ' {
                return line;
            }
        }
    }

    let mut stream = TcpStream::connect(addr).await.expect("连上假 SMTP");
    assert!(read_reply(&mut stream).await.starts_with("220"), "greeting");
    stream.write_all(b"EHLO e2e\r\n").await.unwrap();
    let ehlo = read_reply(&mut stream).await;
    assert!(ehlo.starts_with("250"), "EHLO 被拒: {ehlo}");
    stream.write_all(b"STARTTLS\r\n").await.unwrap();
    let starttls = read_reply(&mut stream).await;
    assert!(starttls.starts_with("220"), "STARTTLS 被拒: {starttls}");

    let connector = tokio_native_tls::TlsConnector::from(
        native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
    );
    let mut tls = connector
        .connect("127.0.0.1", stream)
        .await
        .expect("TLS 握手");
    tls.write_all(b"EHLO e2e\r\n").await.unwrap();
    assert!(read_reply(&mut tls).await.starts_with("250"));
    tls.write_all(b"MAIL FROM:<sender@test.local>\r\n")
        .await
        .unwrap();
    assert!(read_reply(&mut tls).await.starts_with("250"));
    tls.write_all(b"RCPT TO:<rcpt@test.local>\r\n")
        .await
        .unwrap();
    assert!(read_reply(&mut tls).await.starts_with("250"));
    tls.write_all(b"DATA\r\n").await.unwrap();
    let data_reply = read_reply(&mut tls).await;
    assert!(
        data_reply.starts_with("354"),
        "DATA 未获许可（调试）: {data_reply}"
    );
    tls.write_all(b"From: sender@test.local\r\n").await.unwrap();
    tls.write_all(b"To: rcpt@test.local\r\n").await.unwrap();
    tls.write_all(format!("Subject: {OTP_SUBJECT}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    tls.write_all(format!("你的验证码是 {code}\r\n.\r\n").as_bytes())
        .await
        .unwrap();
    assert!(read_reply(&mut tls).await.starts_with("250"), "投递成功");
    tls.write_all(b"QUIT\r\n").await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn managed_account_full_lifecycle_without_touching_toml() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");

    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    let server = FakeMail::start_imap_implicit_and_smtp_starttls()
        .await
        .expect("假服务器");

    // —— 第一次启动：空目录 ——
    let (task, shutdown_tx, socket) = spawn_daemon(&paths).await;
    let client = client_for(&socket);

    // 空安装：无 profile、无 accounts.toml 文件。
    let empty = client.list_account_profiles(true).await.unwrap();
    assert!(empty.profiles.is_empty());
    assert!(!paths.accounts_file.exists(), "空安装不得制造目录文件");

    // revision 冲突在创建前就能被稳定拒绝（错误码契约）。
    let conflict = client
        .update_account("acct_aaaaaaaaaaaaaaaaaaaaaa", 1, &Default::default(), Default::default())
        .await
        .err()
        .unwrap();
    assert_eq!(conflict.rpc_code, Some(-32010));

    // —— probeAccount：双侧就绪且不保存任何状态 ——
    let report = client
        .probe_account(&draft_for(&server), IMAP_PASSWORD, None)
        .await
        .expect("probeAccount 双侧应就绪");
    assert!(report.incoming.ok && report.incoming.stage == "ready");
    let outgoing = report.outgoing.expect("draft 携带发信配置");
    assert!(outgoing.ok && outgoing.stage == "ready");
    assert!(
        !paths.accounts_file.exists(),
        "probe 不保存，目录文件仍不创建"
    );

    // —— 动态添加：probe → 保存凭据 → 发布 → 立即挂载 ——
    let created = client
        .create_account(&draft_for(&server), IMAP_PASSWORD, None)
        .await
        .expect("createAccount 应通过强制探测");
    assert!(created.persisted);
    assert!(created.active);
    let id = created.profile.id.clone();
    assert!(id.starts_with("acct_") && id.len() == 31);
    assert_eq!(created.profile.credentials.incoming, "saved");
    assert_eq!(
        created.profile.credentials.outgoing.as_deref(),
        Some("shared")
    );
    assert!(paths.accounts_file.exists(), "首次保存后应存在 accounts.toml");

    // Agent 不重启即进入 online。
    wait_online(&client, &id).await;

    // —— 收信：经假 SMTP 投递 → IDLE 推送 → 落库（account = 受管 id） ——
    deliver_mail(server.smtp_addr, "135790").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "新邮件未在期限内落库"
        );
        let status = client.status().await.unwrap();
        if status.message_count >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let messages = client.messages(50, 0).await.unwrap();
    assert_eq!(messages.messages[0].account, id, "消息挂在不可变 id 下");

    // —— 发信：sendFromAccount 固定 From，不允许伪造 ——
    client
        .send_from_account(&id, "peer@test.local", "回复", "收到")
        .await
        .expect("从受管账户发信");
    // ADR-0028：中文主题现在以 RFC 2047 encoded-word 出现在报文里，解回来必须一致。
    let sent = server
        .messages()
        .into_iter()
        .find(|raw| {
            mailparse::parse_mail(raw)
                .ok()
                .and_then(|parsed| parsed.headers.get_first_value("Subject"))
                .is_some_and(|subject| subject == "回复")
        })
        .expect("假服务器应收到外发邮件");
    let parsed = mailparse::parse_mail(&sent).expect("外发报文可解析");
    assert_eq!(
        parsed.headers.get_first_value("From").as_deref(),
        Some("rcpt@test.local")
    );
    assert!(
        parsed.headers.get_first_value("Date").is_some(),
        "RFC 5322 要求 Date"
    );
    assert!(
        parsed.headers.get_first_value("Message-ID").is_some(),
        "Message-ID 由我们生成，本地才能把它和会话对上"
    );

    // HTTP/stdio 之外的秘密门禁已在 api 单测覆盖；这里验证 revision 冲突。
    let stale = client
        .update_account(
            &id,
            99,
            &maild_client::UpdatePatchInput {
                display_name: Some("改名".into()),
                ..Default::default()
            },
            Default::default(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(stale.rpc_code, Some(-32011));

    // —— disable：停运行时但保留凭据 ——
    let disabled = client.set_account_enabled(&id, false_revision(&client, &id).await, false).await.unwrap();
    assert!(!disabled.enabled);
    assert_eq!(disabled.runtime.state, "disabled");
    assert_eq!(disabled.credentials.incoming, "saved", "停用保留凭据");

    // —— enable：重新挂载并回到 online ——
    let enabled = client.set_account_enabled(&id, disabled.revision, true).await.unwrap();
    assert!(enabled.enabled);
    wait_online(&client, &id).await;

    // —— 重启：配置与凭据持久化，自动恢复收信 ——
    shutdown_tx.send(()).unwrap();
    task.await.unwrap().expect("第一次运行干净退出");
    let (task2, shutdown_tx2, socket2) = spawn_daemon(&paths).await;
    assert_eq!(socket2, socket);
    let client2 = client_for(&socket2);

    // 无需任何 createAccount：启动即挂载已启用 profile。
    wait_online(&client2, &id).await;
    let after_restart = client2.list_account_profiles(false).await.unwrap();
    assert_eq!(after_restart.profiles.len(), 1);
    assert_eq!(after_restart.profiles[0].revision, enabled.revision);

    // 历史邮件仍在；重启后新邮件继续入库（store 幂等去重兜底重放）。
    let messages = client2.messages(50, 0).await.unwrap();
    assert!(messages.total >= 1);
    deliver_mail(server.smtp_addr, "246810").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(tokio::time::Instant::now() < deadline, "重启后新邮件未落库");
        let status = client2.status().await.unwrap();
        if status.message_count >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // —— remove：软归档 + 删凭据 + 本地邮件保留 ——
    let removed = client2
        .remove_account(&id, after_restart.profiles[0].revision)
        .await
        .unwrap();
    assert!(removed.archived);
    assert!(removed.credentials_deleted);
    assert!(removed.messages_retained);

    let remaining = client2.list_account_profiles(false).await.unwrap();
    assert!(remaining.profiles.is_empty(), "归档后默认列表不含该账户");
    let all = client2.list_account_profiles(true).await.unwrap();
    let archived = all
        .profiles
        .iter()
        .find(|profile| profile.id == id)
        .expect("归档记录保留非秘密资料");
    assert!(archived.archived_at.is_some());
    assert!(!archived.enabled);
    assert_eq!(archived.credentials.incoming, "missing", "归档不保留有效凭据");

    let status = client2.status().await.unwrap();
    assert!(status.message_count >= 2, "本地邮件必须保留");

    // 归档后不可再操作。
    for enabled in [false, true] {
        let gone: ClientError = client2
            .set_account_enabled(&id, archived.revision, enabled)
            .await
            .expect_err("归档账户不得重新启停或挂载 runtime");
        assert_eq!(gone.rpc_code, Some(-32010));
        assert_eq!(gone.kind, ErrorKind::Rpc);
    }
    let gone: ClientError = client2
        .remove_account(&id, archived.revision)
        .await
        .expect_err("归档账户不得重复删除");
    assert_eq!(gone.rpc_code, Some(-32010));
    assert_eq!(gone.kind, ErrorKind::Rpc);

    shutdown_tx2.send(()).unwrap();
    task2.await.unwrap().unwrap();
}

async fn false_revision(client: &RpcClient, id: &str) -> u64 {
    client
        .list_account_profiles(false)
        .await
        .unwrap()
        .profiles
        .iter()
        .find(|profile| profile.id == id)
        .map(|profile| profile.revision)
        .unwrap_or(0)
}

/// JSON 参数形状抽查：deny_unknown_fields 的服务端会拒绝未知字段。
#[tokio::test(flavor = "current_thread")]
async fn account_methods_reject_unknown_params() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    let (_task, shutdown_tx, socket) = spawn_daemon(&paths).await;
    let client = client_for(&socket);

    let error = client
        .call("listAccountProfiles", json!({ "bogus": true }))
        .await
        .err()
        .unwrap();
    assert_eq!(error.rpc_code, Some(-32602));

    shutdown_tx.send(()).unwrap();
}
