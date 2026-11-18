//! M4-06 别名与一次性地址闭环验收（ADR-0022）。
//!
//! 覆盖：地址族 RPC 全循环（create/list/update/remove，disposable 随机
//! 生成、唯一冲突、惰性过期、状态过滤）；别名发信只重写 From（假 SMTP
//! 断言信封头），停用/跨账户地址被拒绝；收信 To 头落到 recipient 列
//! （摘要与详情视图可见）。
//!
//! hermetic：FakeMail + `MAILD_TEST_CREDENTIAL_STORE=memory` +
//! `MAILD_TEST_ACCEPT_INVALID_CERTS=1`（均只在 debug 构建生效）。

mod common;

use std::time::Duration;

use common::fake_mail::FakeMail;
use maild::{run_daemon_until, DaemonOptions};
use maild_client::{
    AccountDraftInput, Endpoint, IncomingInput, OutgoingInput, RpcClient,
};
use mailparse::MailHeaderMap as _;

use maild_paths::AppPaths;
use rusqlite::params;
use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

const IMAP_PASSWORD: &str = "imap-pass-1357";

fn test_paths(root: &std::path::Path) -> AppPaths {
    AppPaths::from_roots(
        &root.join("config"),
        &root.join("data"),
        Some(&root.join("runtime")),
        Some(&root.join("state")),
        root,
    )
}

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

fn draft_for(server: &FakeMail) -> AccountDraftInput {
    AccountDraftInput {
        display_name: "地址层测试".into(),
        address: "owner@test.local".into(),
        incoming: IncomingInput::imap(
            "127.0.0.1",
            server.imap_addr.port(),
            "implicit",
            "addr-user",
            "INBOX",
            true,
            60,
        ),
        outgoing: Some(OutgoingInput::smtp(
            "127.0.0.1",
            server.smtp_addr.port(),
            "starttls",
            "addr-user",
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
            if profile.runtime.state == "online" {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 经假服务器投递一封信；To 头可自定义（地址归属标记的输入）。
async fn deliver_mail(server_addr: &str, to_header: &str, subject: &str, code: &str) {
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

    let mut stream = TcpStream::connect(server_addr).await.expect("连上假服务器");
    assert!(read_reply(&mut stream).await.starts_with("220"));
    stream.write_all(b"EHLO e2e\r\n").await.unwrap();
    assert!(read_reply(&mut stream).await.starts_with("250"));
    stream.write_all(b"STARTTLS\r\n").await.unwrap();
    assert!(read_reply(&mut stream).await.starts_with("220"));

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
    tls.write_all(b"MAIL FROM:<sender@outside.test>\r\n")
        .await
        .unwrap();
    assert!(read_reply(&mut tls).await.starts_with("250"));
    tls.write_all(b"RCPT TO:<owner@test.local>\r\n")
        .await
        .unwrap();
    assert!(read_reply(&mut tls).await.starts_with("250"));
    tls.write_all(b"DATA\r\n").await.unwrap();
    assert!(read_reply(&mut tls).await.starts_with("354"));
    tls.write_all(b"From: sender@outside.test\r\n").await.unwrap();
    tls.write_all(format!("To: {to_header}\r\n").as_bytes())
        .await
        .unwrap();
    tls.write_all(format!("Subject: {subject}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    tls.write_all(format!("你的验证码是 {code}\r\n.\r\n").as_bytes())
        .await
        .unwrap();
    assert!(read_reply(&mut tls).await.starts_with("250"), "投递成功");
    tls.write_all(b"QUIT\r\n").await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn address_lifecycle_alias_send_and_recipient_capture() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");

    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    let server = FakeMail::start_imap_implicit_and_smtp_starttls()
        .await
        .expect("假服务器");

    let (task, shutdown_tx, socket) = spawn_daemon(&paths).await;
    let client = client_for(&socket);

    // 受管账户就绪。
    client
        .probe_account(&draft_for(&server), IMAP_PASSWORD, None)
        .await
        .expect("双侧探测通过");
    let created = client
        .create_account(&draft_for(&server), IMAP_PASSWORD, None)
        .await
        .expect("创建受管账户");
    let account_id = created.profile.id.clone();
    wait_online(&client, &account_id).await;

    let smtp_addr = format!("127.0.0.1:{}", server.smtp_addr.port());

    // —— 创建：alias 显式 + disposable 代生成 ——
    let alias = client
        .call(
            "createAddress",
            json!({
                "account": account_id,
                "kind": "alias",
                "address": "shop@test.local",
                "label": "购物",
            }),
        )
        .await
        .expect("创建 alias");
    assert_eq!(alias["address"], "shop@test.local");
    assert_eq!(alias["kind"], "alias");
    assert_eq!(alias["state"], "active");
    assert!(alias["id"].as_str().unwrap().starts_with("addr_"));
    let alias_id = alias["id"].as_str().unwrap().to_owned();

    let disposable = client
        .call(
            "createAddress",
            json!({ "account": account_id, "kind": "disposable", "expires_secs": 3600 }),
        )
        .await
        .expect("代生成 disposable");
    let disposable_address = disposable["address"].as_str().unwrap().to_owned();
    let (local_part, domain) = disposable_address.split_once('@').unwrap();
    assert_eq!(domain, "test.local", "域名继承父账户");
    assert_eq!(local_part.len(), 12, "随机 local part 为 12 位 base32");
    assert!(local_part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
    assert_eq!(disposable["kind"], "disposable");
    assert!(disposable["expires_at"].as_u64().is_some());

    // 唯一冲突：重复显式地址 → ACCOUNT_CONFLICT。
    let conflict = client
        .call(
            "createAddress",
            json!({ "account": account_id, "kind": "alias", "address": "shop@test.local" }),
        )
        .await
        .expect_err("重复地址必须拒绝");
    assert_eq!(conflict.rpc_code, Some(-32011));

    // 形状校验 fail-closed。
    let malformed = client
        .call(
            "createAddress",
            json!({ "account": account_id, "kind": "alias", "address": "not-an-address" }),
        )
        .await
        .expect_err("非法形状必须拒绝");
    assert_eq!(malformed.rpc_code, Some(-32602));
    let no_address_alias = client
        .call("createAddress", json!({ "account": account_id, "kind": "alias" }))
        .await
        .expect_err("alias 缺地址必须拒绝");
    assert_eq!(no_address_alias.rpc_code, Some(-32602));
    let expiring_alias = client
        .call(
            "createAddress",
            json!({
                "account": account_id,
                "kind": "alias",
                "address": "temporary-alias@test.local",
                "expires_secs": 60,
            }),
        )
        .await
        .expect_err("alias 不得携带 disposable TTL");
    assert_eq!(expiring_alias.rpc_code, Some(-32602));

    // —— listAddresses：active 过滤与 include_inactive ——
    let active_only: serde_json::Value = client
        .call(
            "listAddresses",
            json!({ "account": account_id, "include_inactive": false }),
        )
        .await
        .unwrap();
    assert_eq!(active_only["addresses"].as_array().unwrap().len(), 2);

    // 可选分页保持单帧有界；total 反映过滤后的完整集合。
    let first_page: serde_json::Value = client
        .call(
            "listAddresses",
            json!({
                "account": account_id,
                "include_inactive": false,
                "limit": 1,
                "offset": 0,
            }),
        )
        .await
        .unwrap();
    assert_eq!(first_page["addresses"].as_array().unwrap().len(), 1);
    assert_eq!(first_page["total"], 2);
    let invalid_page = client
        .call(
            "listAddresses",
            json!({ "account": account_id, "limit": 0 }),
        )
        .await
        .expect_err("零 page limit 必须拒绝");
    assert_eq!(invalid_page.rpc_code, Some(-32602));

    // 停用 disposable → active 列表只剩 alias。
    let disabled_view: serde_json::Value = client
        .call(
            "updateAddress",
            json!({ "id": disposable["id"], "state": "disabled" }),
        )
        .await
        .unwrap();
    assert_eq!(disabled_view["state"], "disabled");
    let after_disable: serde_json::Value = client
        .call(
            "listAddresses",
            json!({ "account": account_id, "include_inactive": false }),
        )
        .await
        .unwrap();
    let listed = after_disable["addresses"].as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["address"], "shop@test.local");

    // —— 别名发信：From 重写、凭据仍走父账户 ——
    client
        .send_from_account_with_address(
            &account_id,
            "friend@outside.test",
            "经别名",
            "正文",
            Some("shop@test.local"),
        )
        .await
        .expect("别名发信成功");
    // ADR-0028：中文主题以 RFC 2047 encoded-word 出现在报文里，解回来必须一致。
    let sent = server
        .messages()
        .into_iter()
        .find(|raw| {
            mailparse::parse_mail(raw)
                .ok()
                .and_then(|parsed| parsed.headers.get_first_value("Subject"))
                .is_some_and(|subject| subject == "经别名")
        })
        .expect("假 SMTP 应收到外发邮件");
    let parsed = mailparse::parse_mail(&sent).expect("外发报文可解析");
    assert_eq!(
        parsed.headers.get_first_value("From").as_deref(),
        Some("shop@test.local"),
        "From 必须是别名"
    );

    // 停用地址不能发信。
    let blocked = client
        .send_from_account_with_address(
            &account_id,
            "friend@outside.test",
            "停用后",
            "正文",
            Some(&disposable_address),
        )
        .await
        .expect_err("停用地址发信必须拒绝");
    assert_eq!(blocked.rpc_code, Some(-32602));

    // 不存在的地址拒绝。
    let unknown = client
        .send_from_account_with_address(
            &account_id,
            "friend@outside.test",
            "未知",
            "正文",
            Some("ghost@test.local"),
        )
        .await
        .expect_err("未知别名必须拒绝");
    assert_eq!(unknown.rpc_code, Some(-32602));

    // 跨账户引用也拒绝：即使地址真实存在且 active，也不能借用其他账户
    // 的地址与凭据发信。
    let mut foreign_draft = draft_for(&server);
    foreign_draft.display_name = "另一个账户".into();
    foreign_draft.address = "other@test.local".into();
    let foreign = client
        .create_account(&foreign_draft, IMAP_PASSWORD, None)
        .await
        .expect("创建第二账户");
    wait_online(&client, &foreign.profile.id).await;
    let foreign_alias: serde_json::Value = client
        .call(
            "createAddress",
            json!({
                "account": foreign.profile.id,
                "kind": "alias",
                "address": "foreign@test.local",
            }),
        )
        .await
        .expect("创建其他账户别名");
    let cross_account = client
        .send_from_account_with_address(
            &account_id,
            "friend@outside.test",
            "跨账户",
            "正文",
            foreign_alias["address"].as_str(),
        )
        .await
        .expect_err("跨账户别名必须拒绝");
    assert_eq!(cross_account.rpc_code, Some(-32602));
    client
        .remove_account(&foreign.profile.id, foreign.profile.revision)
        .await
        .expect("删除第二账户并级联地址");

    // —— 收信 recipient 标记：投递到别名的信落库带收件人 ——
    deliver_mail(&smtp_addr, "shop@test.local", "登录验证码", "135790").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let summaries = loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "邮件未在期限内入库"
        );
        let all = client.message_summaries(Some(&account_id), 50, 0).await.unwrap().messages;
        if all.iter().any(|m| m.subject.contains("登录验证码")) {
            break all;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let aliased = summaries
        .iter()
        .find(|m| m.recipient == "shop@test.local")
        .expect("摘要视图应暴露收件别名");
    assert!(aliased.subject.contains("登录验证码"));

    // 详情视图一致。
    let detail = client
        .message(&account_id, &aliased.uid)
        .await
        .unwrap()
        .message;
    assert_eq!(detail.recipient, "shop@test.local");

    // —— removeAddress：删除后 NotFound，消息保留 ——
    client
        .call("removeAddress", json!({ "id": alias_id }))
        .await
        .expect("删除别名");
    let gone = client
        .call(
            "removeAddress",
            json!({ "id": alias_id }),
        )
        .await
        .expect_err("重复删除必须 NotFound");
    assert_eq!(gone.rpc_code, Some(-32010));
    let remaining: serde_json::Value = client
        .call(
            "listAddresses",
            json!({ "account": account_id, "include_inactive": true }),
        )
        .await
        .unwrap();
    assert_eq!(remaining["addresses"].as_array().unwrap().len(), 1);
    // 消息历史保留：recipient 成为悬垂文本。
    let still_there = client
        .message(&account_id, &aliased.uid)
        .await
        .unwrap()
        .message;
    assert_eq!(still_there.recipient, "shop@test.local");

    // removeAccount 级联删除剩余地址，但历史消息的 recipient 文本保留。
    client
        .remove_account(&account_id, created.profile.revision)
        .await
        .expect("删除父账户");
    let all_addresses: serde_json::Value = client
        .call("listAddresses", json!({ "include_inactive": true }))
        .await
        .unwrap();
    assert!(
        all_addresses["addresses"].as_array().unwrap().is_empty(),
        "账户删除后不得遗留地址行"
    );

    // 模拟进程在「catalog 已发布归档、地址 DELETE 尚未执行」之间退出：
    // 重试 remove 必须清掉残留，即使调用方只持有归档前的旧 revision。
    let connection = rusqlite::Connection::open(paths.data_dir.join("mail.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO addresses
             (id, account, address, kind, label, state, created_at, expires_at)
             VALUES (?1, ?2, ?3, 'alias', '', 'active', ?4, NULL)",
            params![
                "addr_interrupted_cleanup",
                account_id,
                "interrupted@test.local",
                1_i64
            ],
        )
        .unwrap();
    drop(connection);
    let retry = client
        .remove_account(&account_id, created.profile.revision)
        .await
        .expect("归档后重试应完成地址级联清理");
    assert!(retry.archived);
    assert!(
        !retry.credentials_deleted,
        "失去 credential revision 后不得把删除状态误报为已确认"
    );
    let after_retry: serde_json::Value = client
        .call("listAddresses", json!({ "include_inactive": true }))
        .await
        .unwrap();
    assert!(
        after_retry["addresses"].as_array().unwrap().is_empty(),
        "归档发布与 DELETE 间中断不得永久遗留地址"
    );

    // 同一崩溃窗口若没有调用方重试，下一次启动也应按 archived catalog
    // 自动收敛，不能要求用户知道隐藏账户 id 才能释放地址唯一值。
    let connection = rusqlite::Connection::open(paths.data_dir.join("mail.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO addresses
             (id, account, address, kind, label, state, created_at, expires_at)
             VALUES (?1, ?2, ?3, 'alias', '', 'active', ?4, NULL)",
            params![
                "addr_startup_cleanup",
                account_id,
                "startup-cleanup@test.local",
                2_i64
            ],
        )
        .unwrap();
    drop(connection);

    let retained = client
        .message(&account_id, &aliased.uid)
        .await
        .unwrap()
        .message;
    assert_eq!(retained.recipient, "shop@test.local");

    let _ = shutdown_tx.send(());
    task.await.unwrap().expect("daemon 正常退出");

    let (task2, shutdown_tx2, socket2) = spawn_daemon(&paths).await;
    let client2 = client_for(&socket2);
    let after_restart: serde_json::Value = client2
        .call("listAddresses", json!({ "include_inactive": true }))
        .await
        .unwrap();
    assert!(
        after_restart["addresses"].as_array().unwrap().is_empty(),
        "启动必须自动收敛归档账户地址残留"
    );
    let retained = client2
        .message(&account_id, &aliased.uid)
        .await
        .unwrap()
        .message;
    assert_eq!(retained.recipient, "shop@test.local");
    let _ = shutdown_tx2.send(());
    task2.await.unwrap().expect("重启后的 daemon 正常退出");
}

fn client_for(socket: &std::path::Path) -> RpcClient {
    RpcClient::new(Endpoint::Unix {
        path: socket.to_path_buf(),
    })
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn expired_disposable_is_lazily_rejected_for_sending() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");

    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    let server = FakeMail::start_imap_implicit_and_smtp_starttls()
        .await
        .expect("假服务器");

    let (task, shutdown_tx, socket) = spawn_daemon(&paths).await;
    let client = client_for(&socket);

    client
        .probe_account(&draft_for(&server), IMAP_PASSWORD, None)
        .await
        .expect("双侧探测通过");
    let created = client
        .create_account(&draft_for(&server), IMAP_PASSWORD, None)
        .await
        .expect("创建受管账户");
    wait_online(&client, &created.profile.id).await;

    // expires_secs=0 在参数层就被拒绝（TTL 必须 ≥1）。
    let zero_ttl = client
        .call(
            "createAddress",
            json!({ "account": created.profile.id, "kind": "disposable", "expires_secs": 0 }),
        )
        .await
        .expect_err("零 TTL 必须拒绝");
    assert_eq!(zero_ttl.rpc_code, Some(-32602));

    // 1 秒 TTL：等过期后惰性判定生效——列表里 state 变 expired，发信拒绝。
    let short_lived = client
        .call(
            "createAddress",
            json!({
                "account": created.profile.id,
                "kind": "disposable",
                "expires_secs": 1,
            }),
        )
        .await
        .unwrap();
    let address = short_lived["address"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(1300)).await;

    let listing: serde_json::Value = client
        .call(
            "listAddresses",
            json!({ "account": created.profile.id, "include_inactive": false }),
        )
        .await
        .unwrap();
    assert_eq!(
        listing["addresses"].as_array().unwrap().len(),
        0,
        "过期地址从 active 列表消失"
    );

    let rejected = client
        .send_from_account_with_address(
            &created.profile.id,
            "friend@outside.test",
            "过期",
            "正文",
            Some(&address),
        )
        .await
        .expect_err("过期地址发信必须拒绝");
    assert_eq!(rejected.rpc_code, Some(-32602));

    let revive = client
        .call(
            "updateAddress",
            json!({ "id": short_lived["id"], "state": "active" }),
        )
        .await
        .expect_err("过期地址不得重新启用");
    assert_eq!(revive.rpc_code, Some(-32602));

    let _ = shutdown_tx.send(());
    task.await.unwrap().expect("daemon 正常退出");
}
