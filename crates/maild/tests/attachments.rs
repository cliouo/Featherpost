//! M4.5-G02 附件字节通道与远程内容策略验收（ADR-0032 实施切片）。
//!
//! 真 daemon + 预置 SQLite。钉住的性质：声明 `messages_read` 的前端主体能跨块取回大附件且字节
//! 逐一相等；四元组任一元不匹配都拒绝且三种「拿不到」可区分；策略是 daemon 状态——重启后仍在、
//! 换主体读到同一结论；只声明 `message` 路由的前端改不了策略；损坏的策略文件不被覆盖。
//! 末尾一条钉住 ADR-0048 的语言偏好：同为 daemon 状态，任何主体可读、`settings_write` 才能改、
//! 改动推给其他主体的长连接。
//!
//! 与 ADR-0030 的验收同一姿态：这里断言的是**协议契约**，不是沙箱。内核不知道渲染层实际加载了
//! 什么，`remote_content` 是结论，执行它是 M4.5-04 的事。

use std::time::Duration;

use maild::api::MAX_ATTACHMENT_CHUNK_BYTES;
use maild::events::{MailAttachment, MailMessage};
use maild::folders::FolderRole;
use maild::store::Store;
use maild::{run_daemon_until, DaemonOptions};
use maild_client::{ClientError, Endpoint, ReadingPolicyPatch, RpcClient};
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

fn write_config(paths: &AppPaths) {
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
}

/// 只声明路由的 v1 前端：`message` → `messages_read`，`settings` → `settings_read + settings_write`。
fn install_frontend(paths: &AppPaths, id: &str, routes: &str) {
    let package = paths.frontend_dir.join(id);
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("frontend.toml"),
        format!(
            "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Attachment fixture\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [{routes}]\n"
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

/// 收件箱那封的大附件：比一块多 3 字节，恰好要三个往返（两满块 + 尾块）。
fn big_bytes() -> Vec<u8> {
    (0..(2 * MAX_ATTACHMENT_CHUNK_BYTES + 3))
        .map(|index| (index.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect()
}

/// daemon 打开数据库之前预置。`Store` 持有数据库锁，必须在 daemon 启动前 drop。
fn seed_store(paths: &AppPaths) {
    let db = paths.data_dir.join("mail.sqlite3");
    let store = Store::open(db.to_str().unwrap()).unwrap();
    for (account, sender) in [
        ("personal", "Newsletter <News@Example.com>"),
        ("work", "boss@corp.example.org"),
    ] {
        store
            .upsert_folder(account, "INBOX", "收件箱", Some(FolderRole::Inbox))
            .unwrap();
        store
            .upsert_folder(account, "Sent", "已发送", Some(FolderRole::Sent))
            .unwrap();
        let big = big_bytes();
        store
            .insert_message(&MailMessage {
                account: account.into(),
                uid: "10".into(),
                sender: sender.into(),
                recipient: "me@example.test".into(),
                subject: format!("{account} 收件箱"),
                body: "正文".into(),
                body_html: "<p>版式 <img src=\"cid:logo@maild\"></p>".into(),
                received_at: 1_757_600_000,
                folder: "INBOX".into(),
                has_attachments: true,
                attachments: vec![
                    MailAttachment {
                        part_index: 2,
                        filename: "季度报告.pdf".into(),
                        mime_type: "application/pdf".into(),
                        size: big.len() as u64,
                        stored: true,
                        content: Some(big),
                        ..Default::default()
                    },
                    MailAttachment {
                        part_index: 3,
                        filename: "logo.png".into(),
                        mime_type: "image/png".into(),
                        content_id: "logo@maild".into(),
                        inline: true,
                        size: 4,
                        stored: true,
                        content: Some(b"png!".to_vec()),
                    },
                    MailAttachment {
                        part_index: 4,
                        filename: "raw-footage.mov".into(),
                        mime_type: "video/quicktime".into(),
                        size: 40 * 1024 * 1024,
                        stored: false,
                        content: None,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            })
            .unwrap();
        // 同 uid 的已发送那封：另一封信、另一份字节。
        store
            .insert_message(&MailMessage {
                account: account.into(),
                uid: "10".into(),
                sender: "me@example.test".into(),
                recipient: "someone@example.test".into(),
                subject: format!("{account} 已发送"),
                body: "正文".into(),
                received_at: 1_757_600_100,
                folder: "Sent".into(),
                has_attachments: true,
                attachments: vec![MailAttachment {
                    part_index: 2,
                    filename: "sent.txt".into(),
                    mime_type: "text/plain".into(),
                    size: 9,
                    stored: true,
                    content: Some(b"sent-copy".to_vec()),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .unwrap();
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

async fn wait_socket_or_fail(task: &mut DaemonTask, socket: &std::path::Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        if tokio::time::Instant::now() >= deadline {
            let error = (&mut *task).await.expect("daemon task").err();
            panic!("socket 未在 10s 内出现；daemon 错误: {error:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn stop_daemon(task: DaemonTask, shutdown: tokio::sync::oneshot::Sender<()>) {
    let _ = shutdown.send(());
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("daemon 在 10s 内退出")
        .expect("daemon task")
        .expect("daemon 干净退出");
}

fn unix_client(socket: &std::path::Path) -> RpcClient {
    RpcClient::new(Endpoint::Unix {
        path: socket.to_path_buf(),
    })
    .expect("受管 socket 连接")
}

/// ADR-0030 决策 6 的 bootstrap：先以 user 取回并轮换 token，再开一条连接认证。
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

fn code_of(result: Result<serde_json::Value, ClientError>, what: &str) -> i64 {
    match result {
        Ok(value) => panic!("{what} 本应失败，却返回 {value}"),
        Err(error) => error
            .rpc_code
            .unwrap_or_else(|| panic!("{what} 不是 RPC 错误: {error:?}")),
    }
}

/// `getMessage` 应答里的策略结论（`message` 的同级字段，ADR-0032 决策 5）。
async fn remote_decision(
    connection: &mut maild_client::StreamConnection,
    account: &str,
) -> serde_json::Value {
    call_on(
        connection,
        "getMessage",
        serde_json::json!({ "account": account, "folder": "INBOX", "uid": "10" }),
    )
    .await
    .expect("可读详情")["remote_content"]
        .clone()
}

fn read_params(
    account: &str,
    folder: &str,
    uid: &str,
    part: u32,
    offset: u64,
) -> serde_json::Value {
    serde_json::json!({
        "account": account, "folder": folder, "uid": uid, "part_index": part, "offset": offset
    })
}

#[tokio::test(flavor = "current_thread")]
async fn a_reader_streams_a_large_attachment_in_bounded_frames_and_gets_the_same_bytes() {
    use base64::Engine as _;
    use maild_client::MAX_FRAME_BYTES;

    use_memory_credential_store();
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    write_config(&paths);
    install_frontend(&paths, "reader.fixture", "\"message\"");
    seed_store(&paths);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let client = unix_client(&socket);
    let mut reader = frontend_connection(&client, "reader.fixture").await;

    // 元数据先说实话：pdf 与内联图已存，40 MiB 的视频没存。
    let detail = call_on(
        &mut reader,
        "getMessage",
        serde_json::json!({ "account": "personal", "folder": "INBOX", "uid": "10" }),
    )
    .await
    .expect("messages_read 可读详情");
    let attachments = detail["message"]["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 3);
    assert_eq!(attachments[0]["stored"], true);
    assert_eq!(attachments[1]["stored"], true);
    assert_eq!(attachments[2]["stored"], false);
    assert!(
        attachments[0].get("content").is_none(),
        "字节不随 getMessage 走"
    );

    // 跨块取回：每一帧都落在单帧上限内，拼起来逐字节相等，往返次数恰好是 ceil(size / chunk)。
    let expected = big_bytes();
    let mut collected: Vec<u8> = Vec::new();
    let mut round_trips = 0usize;
    loop {
        let chunk = call_on(
            &mut reader,
            "readAttachment",
            read_params("personal", "INBOX", "10", 2, collected.len() as u64),
        )
        .await
        .expect("messages_read 可读字节");
        round_trips += 1;
        let frame = serde_json::to_vec(&chunk).unwrap();
        assert!(
            frame.len() <= MAX_FRAME_BYTES,
            "第 {round_trips} 块 {} 字节超帧",
            frame.len()
        );
        assert_eq!(chunk["offset"].as_u64().unwrap(), collected.len() as u64);
        assert_eq!(
            chunk["attachment"]["size"].as_u64().unwrap(),
            expected.len() as u64
        );
        let data = base64::engine::general_purpose::STANDARD
            .decode(chunk["data"].as_str().unwrap())
            .unwrap();
        assert!(
            data.len() <= MAX_ATTACHMENT_CHUNK_BYTES,
            "单块原始字节不得超过上限"
        );
        assert_eq!(data.len() as u64, chunk["length"].as_u64().unwrap());
        collected.extend(data);
        if chunk["eof"].as_bool().unwrap() {
            break;
        }
        assert!(round_trips < 10, "块循环没有终止");
    }
    assert_eq!(round_trips, 3, "两满块 + 一个 3 字节尾块");
    assert_eq!(collected.len(), expected.len());
    assert!(collected == expected, "跨块拼装后字节必须逐一相等");

    // 共享 client 的循环拼装得到同一份字节，且带回元数据。
    let assembled = client
        .read_attachment("personal", "INBOX", "10", 2)
        .await
        .expect("client 循环拼装");
    assert!(assembled.bytes == expected);
    assert_eq!(assembled.attachment.filename, "季度报告.pdf");
    assert!(assembled.attachment.stored);

    // 内联图（`cid:` 的目标）走同一条通道，按 part_index 定位。
    let inline = client
        .read_attachment("personal", "INBOX", "10", 3)
        .await
        .unwrap();
    assert_eq!(inline.bytes, b"png!");
    assert_eq!(inline.attachment.content_id, "logo@maild");

    // 决策 2：同 uid 的已发送那封是另一封信。
    let sent = client
        .read_attachment("personal", "Sent", "10", 2)
        .await
        .unwrap();
    assert_eq!(sent.bytes, b"sent-copy");

    drop(reader);
    stop_daemon(task, shutdown).await;
}

#[tokio::test(flavor = "current_thread")]
async fn mismatched_keys_and_unstored_bytes_are_refused_distinctly() {
    use_memory_credential_store();
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    write_config(&paths);
    install_frontend(&paths, "reader.fixture", "\"message\"");
    seed_store(&paths);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let client = unix_client(&socket);
    let mut reader = frontend_connection(&client, "reader.fixture").await;

    // 决策 3：三种「拿不到」各有其名。
    let cases: &[(&str, serde_json::Value, i64)] = &[
        ("错账户", read_params("nobody", "INBOX", "10", 2, 0), -32002),
        (
            "错文件夹",
            read_params("personal", "Archive", "10", 2, 0),
            -32002,
        ),
        (
            "错 uid",
            read_params("personal", "INBOX", "11", 2, 0),
            -32002,
        ),
        (
            "错 part",
            read_params("personal", "INBOX", "10", 7, 0),
            -32003,
        ),
        (
            "已发送那封没有 part 3",
            read_params("personal", "Sent", "10", 3, 0),
            -32003,
        ),
        (
            "超限未存",
            read_params("personal", "INBOX", "10", 4, 0),
            -32004,
        ),
        (
            "offset 越界",
            read_params("personal", "INBOX", "10", 3, 5),
            -32602,
        ),
        (
            "folder 缺省不消歧",
            serde_json::json!({ "account": "personal", "uid": "10", "part_index": 2 }),
            -32602,
        ),
        (
            "length 超上限",
            serde_json::json!({
                "account": "personal", "folder": "INBOX", "uid": "10", "part_index": 2,
                "length": MAX_ATTACHMENT_CHUNK_BYTES + 1
            }),
            -32602,
        ),
    ];
    for (what, params, expected) in cases {
        let code = code_of(
            call_on(&mut reader, "readAttachment", params.clone()).await,
            what,
        );
        assert_eq!(code, *expected, "{what}");
    }

    // 跨账户：work 的收件箱 uid 10 是另一封信，字节是它自己的——不是 personal 的那份。
    let work = client
        .read_attachment("work", "INBOX", "10", 3)
        .await
        .unwrap();
    assert_eq!(work.bytes, b"png!");
    assert_eq!(
        code_of(
            client
                .read_attachment("work", "INBOX", "10", 4)
                .await
                .map(|_| serde_json::Value::Null),
            "work 超限"
        ),
        -32004
    );

    // 共享 client 的护栏：对 -32004 不会拼出一个空文件来。
    let error = client
        .read_attachment("personal", "INBOX", "10", 4)
        .await
        .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32004));

    drop(reader);
    stop_daemon(task, shutdown).await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_remote_content_policy_is_daemon_state_shared_by_every_principal() {
    use_memory_credential_store();
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    write_config(&paths);
    install_frontend(&paths, "reader.fixture", "\"message\"");
    install_frontend(&paths, "settings.fixture", "\"message\", \"settings\"");
    seed_store(&paths);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let client = unix_client(&socket);
    let mut reader = frontend_connection(&client, "reader.fixture").await;
    let mut settings = frontend_connection(&client, "settings.fixture").await;

    // 缺省：阻止，理由 default；文件尚未创建。
    let policy = client.reading_policy().await.unwrap();
    assert_eq!(policy.policy.remote_content, "block");
    assert!(policy.policy.trusted_senders.is_empty());
    assert!(policy.persisted);
    assert!(
        !paths.config_dir.join("reading_policy.toml").exists(),
        "只读从不落盘"
    );
    assert_eq!(
        remote_decision(&mut reader, "personal").await,
        serde_json::json!({ "decision": "block", "reason": "default" })
    );

    // 只声明 `message` 路由的前端：能读策略结论（它在 getMessage 里），改不了策略。
    assert_eq!(
        code_of(
            call_on(
                &mut reader,
                "setReadingPolicy",
                serde_json::json!({ "trust": ["news@example.com"] })
            )
            .await,
            "reader 改策略"
        ),
        -32016
    );
    assert_eq!(
        code_of(
            call_on(&mut reader, "getReadingPolicy", serde_json::json!({})).await,
            "reader 读策略"
        ),
        -32016
    );

    // 声明 `settings` 路由的前端可以改；结论对**另一个主体**同步翻转——策略不是某个前端的本地状态。
    let updated = call_on(
        &mut settings,
        "setReadingPolicy",
        serde_json::json!({ "trust": ["NEWS@example.com"] }),
    )
    .await
    .expect("settings_write 可改策略");
    assert_eq!(
        updated["policy"]["trusted_senders"][0]["value"],
        "news@example.com"
    );
    assert_eq!(updated["persisted"], true);
    assert_eq!(
        remote_decision(&mut reader, "personal").await,
        serde_json::json!({ "decision": "allow", "reason": "trusted_address" })
    );
    assert_eq!(
        remote_decision(&mut reader, "work").await,
        serde_json::json!({ "decision": "block", "reason": "default" }),
        "别的发件人不受影响"
    );

    // 精确域名：corp.example.org 放行 boss@corp.example.org，但不放行 example.org 的子域以外。
    client
        .set_reading_policy(&ReadingPolicyPatch {
            trust: vec!["Corp.Example.org".into()],
            untrust: vec!["news@example.com".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        remote_decision(&mut reader, "work").await,
        serde_json::json!({ "decision": "allow", "reason": "trusted_domain" })
    );
    assert_eq!(
        remote_decision(&mut reader, "personal").await["decision"],
        "block",
        "untrust 真的删"
    );

    // 整包拒绝：一项非法，好的那项也不落。
    let error = client
        .set_reading_policy(&ReadingPolicyPatch {
            trust: vec!["ok@example.com".into(), "not an address".into()],
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32602));
    let policy = client.reading_policy().await.unwrap();
    assert_eq!(policy.policy.trusted_senders.len(), 1);
    assert_eq!(policy.policy.trusted_senders[0].value, "corp.example.org");
    assert_eq!(policy.policy.trusted_senders[0].kind, "domain");

    // 全局放行：理由 allow_all，名单不再是决定因素。
    client
        .set_reading_policy(&ReadingPolicyPatch {
            remote_content: Some("allow".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        remote_decision(&mut reader, "personal").await,
        serde_json::json!({ "decision": "allow", "reason": "allow_all" })
    );

    drop(reader);
    drop(settings);
    stop_daemon(task, shutdown).await;

    // 重启 = 策略仍在（daemon 状态，不是进程内存）。
    let (mut task, shutdown) = spawn_daemon(&paths);
    wait_socket_or_fail(&mut task, &socket).await;
    let client = unix_client(&socket);
    let policy = client.reading_policy().await.unwrap();
    assert_eq!(policy.policy.remote_content, "allow");
    assert_eq!(policy.policy.trusted_senders.len(), 1);
    assert!(policy.persisted);
    let detail = client
        .message_in_folder("work", "INBOX", "10")
        .await
        .unwrap();
    assert_eq!(detail.remote_content.as_ref().unwrap().reason, "allow_all");
    assert!(detail.remote_content.unwrap().allows());
    stop_daemon(task, shutdown).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_corrupt_policy_file_stalls_to_defaults_and_is_never_overwritten() {
    use_memory_credential_store();
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    write_config(&paths);
    seed_store(&paths);
    let policy_path = paths.config_dir.join("reading_policy.toml");
    let corrupt = "format_version = 99\nremote_content = \"allow\"\n";
    std::fs::write(&policy_path, corrupt).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&policy_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let client = unix_client(&socket);

    // 读到的是缺省（阻止），不是文件里那个未来版本声称的 allow；并且如实报 persisted = false。
    let policy = client.reading_policy().await.unwrap();
    assert_eq!(policy.policy.remote_content, "block");
    assert!(!policy.persisted);
    let detail = client
        .message_in_folder("personal", "INBOX", "10")
        .await
        .unwrap();
    assert_eq!(detail.remote_content.unwrap().decision, "block");

    // 写被拒绝，文件一个字节都没动。
    let error = client
        .set_reading_policy(&ReadingPolicyPatch {
            trust: vec!["news@example.com".into()],
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32001));
    assert_eq!(std::fs::read_to_string(&policy_path).unwrap(), corrupt);

    // 其余功能不受影响：附件照读。
    let inline = client
        .read_attachment("personal", "INBOX", "10", 3)
        .await
        .unwrap();
    assert_eq!(inline.bytes, b"png!");

    stop_daemon(task, shutdown).await;
}

/// ADR-0048 决策 1/2：语言偏好同样是 daemon 状态。`getLocalePreference` 对任何主体开放；改偏好要
/// `settings_write`；改动经 `preferences/locale` 推给其他主体的长连接。
#[tokio::test(flavor = "current_thread")]
async fn the_locale_preference_is_daemon_state_pushed_to_every_stream() {
    use_memory_credential_store();
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    write_config(&paths);
    install_frontend(&paths, "reader.fixture", "\"message\"");
    install_frontend(&paths, "settings.fixture", "\"message\", \"settings\"");
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let client = unix_client(&socket);
    let mut reader = frontend_connection(&client, "reader.fixture").await;
    let mut settings = frontend_connection(&client, "settings.fixture").await;
    let preference_file = paths.config_dir.join("locale_preference.toml");

    // 缺省 system：effective 来自真实系统值，只断言非空；文件尚未创建。
    let current = client.locale_preference().await.unwrap();
    assert_eq!(current.preference.mode, "system");
    assert!(current.preference.languages.is_empty());
    assert!(!current.effective.is_empty(), "effective 永不为空");
    assert!(current.persisted);
    assert!(!preference_file.exists(), "只读从不落盘");

    // 只声明 `message` 路由的前端：能读（Open），不能改。
    let read = call_on(&mut reader, "getLocalePreference", serde_json::json!({}))
        .await
        .expect("任何主体可读语言偏好");
    assert_eq!(read["preference"]["mode"], "system");
    assert_eq!(
        code_of(
            call_on(
                &mut reader,
                "setLocalePreference",
                serde_json::json!({ "mode": "explicit", "languages": ["en"] })
            )
            .await,
            "reader 改语言偏好"
        ),
        -32016
    );

    let updated = call_on(
        &mut settings,
        "setLocalePreference",
        serde_json::json!({ "mode": "explicit", "languages": ["EN-gb", "zh-hans"] }),
    )
    .await
    .expect("settings_write 可改语言偏好");
    assert_eq!(updated["effective"], serde_json::json!(["en-GB", "zh-Hans"]));
    assert!(preference_file.exists(), "首次写入才创建");

    // 另一个主体的长连接收到推送。
    let pushed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let maild_client::Incoming::Notification(notification) =
                reader.next_incoming().await.expect("连接仍在")
            {
                if let Some(changed) = notification.locale_preference_changed() {
                    return changed.expect("推送参数合法");
                }
            }
        }
    })
    .await
    .expect("preferences/locale 应送达");
    assert_eq!(pushed.effective, ["en-GB", "zh-Hans"]);

    // 共享 client 的 typed 调用读到同一份。
    let current = client.locale_preference().await.unwrap();
    assert_eq!(current.preference.mode, "explicit");
    assert_eq!(current.effective, ["en-GB", "zh-Hans"]);
    let reset = client
        .set_locale_preference(&maild_client::LocalePreferenceUpdate::system())
        .await
        .unwrap();
    assert_eq!(reset.preference.mode, "system");

    drop(reader);
    drop(settings);
    stop_daemon(task, shutdown).await;
}
