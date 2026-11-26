//! 联系人自动补全（`searchContacts`）后端验收测试：
//! - 双账户严格隔离（近期样本与 FTS5 前缀回退均无跨账户泄露）
//! - ADR-0034 边界：`drafts` 表绝不读取，`spam`/`trash`/`drafts` 文件夹与 `deleted=1`/`pending_removal=1` 全部排除
//! - 权限矩阵：`messages_read` 放行，`drafts_read`-only 与无权限主体拒绝（`-32016`）
//! - RPC 参数严格校验（`account` 必填、未知键拒绝、`query` 长度与控制字符限制、`limit` 默认 8 / 钳制 20）
//! - `Sent` 收件人加权、己方别名降权、`mailparse` 组/带逗号引号名、截断头保护、RFC 5321 大小写
//! - 正文/无关同收件人不误命中、超近期窗口的 FTS5 前缀回退、SQLite 索引计划与时延验证

use std::time::{Duration, Instant};

use maild::contacts::{MAX_CONTACT_QUERY_BYTES, RECENT_CONTACT_SAMPLE_LIMIT};
use maild::drafts::DraftPatch;
use maild::events::MailMessage;
use maild::folders::FolderRole;
use maild::pending::MessageFlag;
use maild::store::Store;
use maild::{run_daemon_until, DaemonOptions};
use maild_client::{Endpoint, RpcClient, SearchContactsRequest};
use maild_paths::AppPaths;
use serde_json::json;

const BASE: u64 = 1_750_000_000;

fn msg(
    account: &str,
    folder: &str,
    uid: &str,
    sender: &str,
    recipient: &str,
    cc: &str,
    sort_at: u64,
) -> MailMessage {
    MailMessage {
        account: account.into(),
        uid: uid.into(),
        sender: sender.into(),
        recipient: recipient.into(),
        cc: cc.into(),
        subject: format!("subject {uid}"),
        body: format!("body {uid}"),
        received_at: sort_at,
        date_at: Some(sort_at),
        folder: folder.into(),
        ..Default::default()
    }
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

fn install_frontend(paths: &AppPaths, id: &str, routes: &str, permissions: Option<&str>) {
    let package = paths.frontend_dir.join(id);
    std::fs::create_dir_all(&package).unwrap();
    let (schema_version, perms) = match permissions {
        Some(values) => (2, format!("permissions = [{values}]\n")),
        None => (1, String::new()),
    };
    std::fs::write(
        package.join("frontend.toml"),
        format!(
            "schema_version = {schema_version}\nprotocol_version = 1\nid = \"{id}\"\nname = \"Contacts test\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [{routes}]\n{perms}"
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
    let options = DaemonOptions::new(paths.config_file.clone(), paths.clone()).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run_daemon_until(options, async move {
        let _ = rx.await;
    }));
    (task, tx)
}

async fn wait_socket_or_fail(task: &mut DaemonTask, socket: &std::path::Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        if tokio::time::Instant::now() >= deadline {
            let err = (&mut *task).await.unwrap().err();
            panic!("socket 未就绪: {err:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn authed_frontend_connection(
    socket: &std::path::Path,
    frontend_id: &str,
) -> maild_client::StreamConnection {
    let user = RpcClient::new(Endpoint::Unix {
        path: socket.to_path_buf(),
    })
    .unwrap();
    let token = user
        .reveal_plugin_token(frontend_id, Some("frontend"))
        .await
        .expect("受管前端有身份条目");
    let mut conn = user.connect_stream().await.unwrap();
    conn.authenticate(&token).await.expect("认证成功");
    let who = conn.call("whoami", json!({}), |_| {}).await.unwrap();
    assert_eq!(who["principal"], format!("frontend:{frontend_id}"));
    conn
}

#[test]
fn isolates_accounts_and_excludes_deleted_pending_spam_trash_and_drafts() {
    let store = Store::open("").unwrap();

    for account in ["acc_a", "acc_b"] {
        store
            .upsert_folder(account, "INBOX", "INBOX", Some(FolderRole::Inbox))
            .unwrap();
        store
            .upsert_folder(account, "Sent", "Sent", Some(FolderRole::Sent))
            .unwrap();
        store
            .upsert_folder(account, "Spam", "Spam", Some(FolderRole::Spam))
            .unwrap();
        store
            .upsert_folder(account, "Trash", "Trash", Some(FolderRole::Trash))
            .unwrap();
        store
            .upsert_folder(account, "Drafts", "Drafts", Some(FolderRole::Drafts))
            .unwrap();
    }

    // Account A 可见消息
    store
        .insert_message(&msg(
            "acc_a",
            "INBOX",
            "1",
            "Alice A <alice.a@example.com>",
            "me.a@example.com",
            "Carol A <carol.a@example.com>",
            BASE,
        ))
        .unwrap();
    // Account A 已删标记消息（deleted = 1）
    let mut deleted_msg = msg(
        "acc_a",
        "INBOX",
        "2",
        "Deleted Person <deleted@example.com>",
        "me.a@example.com",
        "",
        BASE + 100,
    );
    deleted_msg.deleted = true;
    store.insert_message(&deleted_msg).unwrap();

    // Account A 待移除消息（pending_removal = 1，经 queue_move 置位）
    store
        .insert_message(&msg(
            "acc_a",
            "INBOX",
            "3",
            "Pending Person <pending@example.com>",
            "me.a@example.com",
            "",
            BASE + 200,
        ))
        .unwrap();
    store
        .queue_move_ops("acc_a", "INBOX", &["3".into()], "Trash", BASE + 201)
        .unwrap();

    // Account A spam / trash / drafts 文件夹消息
    store
        .insert_message(&msg(
            "acc_a",
            "Spam",
            "4",
            "Spammer <spam@evil.test>",
            "me.a@example.com",
            "",
            BASE + 300,
        ))
        .unwrap();
    store
        .insert_message(&msg(
            "acc_a",
            "Trash",
            "5",
            "Trashed <trashed@example.com>",
            "me.a@example.com",
            "",
            BASE + 400,
        ))
        .unwrap();
    store
        .insert_message(&msg(
            "acc_a",
            "Drafts",
            "6",
            "me.a@example.com",
            "Folder Draft <folder.draft@example.com>",
            "",
            BASE + 500,
        ))
        .unwrap();

    // Account A `drafts` 表草稿（含独有 Bcc 地址，ADR-0034 严禁通过 messages_read 泄露）
    store
        .create_draft(
            "draft-secret-1",
            "acc_a",
            &DraftPatch {
                to: Some(vec!["draft.only.to@secret.test".into()]),
                cc: Some(vec!["draft.only.cc@secret.test".into()]),
                bcc: Some(vec!["draft.only.bcc@secret.test".into()]),
                subject: Some("secret".into()),
                ..Default::default()
            },
            BASE + 600,
        )
        .unwrap();

    // Account B 可见消息
    store
        .insert_message(&msg(
            "acc_b",
            "Sent",
            "1",
            "me.b@example.com",
            "Bob B <bob.b@example.com>",
            "",
            BASE + 700,
        ))
        .unwrap();

    // 查 Account A（空查询）
    let a_all = store
        .search_contacts("acc_a", "", &["me.a@example.com".into()], 20)
        .unwrap();
    let a_addrs: Vec<&str> = a_all.iter().map(|c| c.address.as_str()).collect();
    assert!(a_addrs.contains(&"alice.a@example.com"));
    assert!(a_addrs.contains(&"carol.a@example.com"));
    for forbidden in [
        "deleted@example.com",
        "pending@example.com",
        "spam@evil.test",
        "trashed@example.com",
        "folder.draft@example.com",
        "draft.only.to@secret.test",
        "draft.only.cc@secret.test",
        "draft.only.bcc@secret.test",
        "bob.b@example.com",
    ] {
        assert!(
            !a_addrs.contains(&forbidden),
            "Account A 不得包含 {forbidden}: {a_addrs:?}"
        );
    }

    // 针对所有受禁地址显式发前缀查询（同时验证 Stage 2 FTS5 回退也不会泄露）
    for q in ["bob", "deleted", "pending", "spam", "trashed", "draft", "secret"] {
        let hits = store
            .search_contacts("acc_a", q, &["me.a@example.com".into()], 20)
            .unwrap();
        assert!(
            hits.is_empty(),
            "Account A 查询 {q:?} 不得泄露受禁或跨账户地址: {hits:?}"
        );
    }

    // 查 Account B：只能看到 Account B，绝看不到 Account A
    let b_all = store
        .search_contacts("acc_b", "", &["me.b@example.com".into()], 20)
        .unwrap();
    let b_addrs: Vec<&str> = b_all.iter().map(|c| c.address.as_str()).collect();
    assert_eq!(b_addrs, vec!["bob.b@example.com", "me.b@example.com"]);
    assert!(store
        .search_contacts("acc_b", "alice", &["me.b@example.com".into()], 20)
        .unwrap()
        .is_empty());

    // 将可见消息改为 deleted = 1（通过 queue_flag_ops）后立刻不可见
    store
        .queue_flag_ops(
            "acc_a",
            "INBOX",
            &["1".into()],
            MessageFlag::Deleted,
            true,
            BASE + 800,
        )
        .unwrap();
    assert!(store
        .search_contacts("acc_a", "alice", &[], 20)
        .unwrap()
        .is_empty());
}

#[test]
fn fallback_prefix_finds_older_history_without_body_or_corecipient_leakage() {
    let store = Store::open("").unwrap();
    store
        .upsert_folder("acc", "INBOX", "INBOX", Some(FolderRole::Inbox))
        .unwrap();

    // 插入一条很早的历史消息，且带同收件人与正文中的无关邮箱。
    let mut old = msg(
        "acc",
        "INBOX",
        "old-1",
        "Old Friend <oldfriend@legacy.org>",
        "me@example.com, Bystander <bystander@other.org>",
        "",
        BASE - 10_000_000,
    );
    old.body = "Please contact bodyonly@hidden.org about oldfriend details".into();
    store.insert_message(&old).unwrap();

    // 插入超过 RECENT_CONTACT_SAMPLE_LIMIT 的更新消息，将 old-1 挤出阶段 1 近期样本。
    for i in 0..(RECENT_CONTACT_SAMPLE_LIMIT + 50) {
        store
            .insert_message(&msg(
                "acc",
                "INBOX",
                &format!("new-{i}"),
                &format!("recent{i}@noise.test"),
                "me@example.com",
                "",
                BASE + i as u64,
            ))
            .unwrap();
    }

    // 阶段 1 中没有 oldfriend，阶段 2 FTS5 前缀回退命中且不把 bystander 带进来。
    let hits = store
        .search_contacts("acc", "oldf", &["me@example.com".into()], 8)
        .unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].address, "oldfriend@legacy.org");
    assert_eq!(hits[0].name.as_deref(), Some("Old Friend"));

    // 仅出现在正文里的地址绝不命中。
    let body_hits = store
        .search_contacts("acc", "bodyonly", &["me@example.com".into()], 8)
        .unwrap();
    assert!(body_hits.is_empty(), "{body_hits:?}");

    // FTS5 语法字符作为查询不得报错。
    for weird in ["\"", "oldf* OR subject:noise", "NEAR(oldf, x)", "-oldf", "(oldf"] {
        assert!(store.search_contacts("acc", weird, &[], 8).is_ok());
    }
}

#[test]
fn bounded_sample_stays_fast_on_large_mailbox() {
    let store = Store::open("").unwrap();
    store
        .upsert_folder("acc", "INBOX", "INBOX", Some(FolderRole::Inbox))
        .unwrap();
    store
        .upsert_folder("acc", "Sent", "Sent", Some(FolderRole::Sent))
        .unwrap();

    for i in 0..2500 {
        let folder = if i % 10 == 0 { "Sent" } else { "INBOX" };
        store
            .insert_message(&msg(
                "acc",
                folder,
                &format!("m-{i}"),
                &format!("Sender {i} <sender{i}@example.test>"),
                &format!("Peer {} <peer{}@example.test>", i % 100, i % 100),
                "",
                BASE + i as u64,
            ))
            .unwrap();
    }

    let mut durations = Vec::with_capacity(20);
    for _ in 0..20 {
        let start = Instant::now();
        let res = store
            .search_contacts("acc", "peer1", &["me@example.test".into()], 8)
            .unwrap();
        durations.push(start.elapsed());
        assert!(!res.is_empty());
        assert!(res.len() <= 8);
    }
    durations.sort();
    let p95 = durations[durations.len() * 95 / 100];
    assert!(
        p95 < Duration::from_millis(250),
        "search_contacts P95 过高: {p95:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn rpc_auth_matrix_param_validation_and_client_dto_roundtrip() {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    let dir = tempfile::tempdir().unwrap();
    let paths = test_paths(dir.path());
    write_config(&paths);

    // 安装 3 个前端：
    // 1. reader：有 inbox（→ messages_read），允许 searchContacts
    // 2. drafts_only：仅有 drafts_read + drafts_write（无 messages_read），必须被拒 -32016
    // 3. bare：仅有 background（无 messages_read），必须被拒 -32016
    install_frontend(&paths, "test.reader", "\"inbox\"", None);
    install_frontend(
        &paths,
        "test.drafts.only",
        "\"background\"",
        Some("\"drafts_read\", \"drafts_write\""),
    );
    install_frontend(&paths, "test.bare", "\"background\"", None);

    // 预置数据库
    {
        let db = paths.data_dir.join("mail.sqlite3");
        let store = Store::open(db.to_str().unwrap()).unwrap();
        store
            .upsert_folder("personal", "INBOX", "INBOX", Some(FolderRole::Inbox))
            .unwrap();
        store
            .upsert_folder("personal", "Sent", "Sent", Some(FolderRole::Sent))
            .unwrap();
        store
            .upsert_folder("work", "INBOX", "INBOX", Some(FolderRole::Inbox))
            .unwrap();

        store
            .insert_message(&msg(
                "personal",
                "Sent",
                "1",
                "me@personal.test",
                r#""Doe, Jane" <Jane.Doe@Example.COM>, Team: Alice <alice@example.com>, bad..addr@x.com;"#,
                "",
                BASE + 100,
            ))
            .unwrap();
        store
            .insert_message(&msg(
                "personal",
                "INBOX",
                "2",
                "Bob <bob@example.com>",
                "me@personal.test",
                "",
                BASE + 50,
            ))
            .unwrap();
        store
            .insert_message(&msg(
                "work",
                "INBOX",
                "1",
                "Boss <boss@work.test>",
                "me@work.test",
                "",
                BASE + 200,
            ))
            .unwrap();
        // 插入一个草稿，确保 searchContacts 绝不泄露
        store
            .create_draft(
                "d1",
                "personal",
                &DraftPatch {
                    bcc: Some(vec!["hidden.bcc@secret.test".into()]),
                    ..Default::default()
                },
                BASE + 300,
            )
            .unwrap();
    }

    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;

    let mut reader = authed_frontend_connection(&socket, "test.reader").await;
    let mut drafts_only = authed_frontend_connection(&socket, "test.drafts.only").await;
    let mut bare = authed_frontend_connection(&socket, "test.bare").await;
    let user = RpcClient::new(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();

    // 1. 权限矩阵：drafts_only（仅有 drafts_read/drafts_write）与 bare（无权限）均返回 -32016
    for denied_conn in [&mut drafts_only, &mut bare] {
        let err = denied_conn
            .call("searchContacts", json!({ "account": "personal" }), |_| {})
            .await
            .unwrap_err();
        assert_eq!(err.rpc_code, Some(-32016), "{err:?}");
        assert!(
            err.message.contains("messages_read"),
            "拒权原因应明确指向缺失 messages_read: {err:?}"
        );
    }

    // 2. messages_read 前端与 user 均可正常调用
    let reader_raw = reader
        .call(
            "searchContacts",
            json!({ "account": "personal", "limit": 8 }),
            |_| {},
        )
        .await
        .unwrap();
    let result: maild_client::SearchContactsResult =
        serde_json::from_value(reader_raw).unwrap();
    let addrs: Vec<&str> = result.contacts.iter().map(|c| c.address.as_str()).collect();
    assert!(addrs.contains(&"Jane.Doe@example.com"), "{result:?}");
    assert!(addrs.contains(&"alice@example.com"), "{result:?}");
    assert!(addrs.contains(&"bob@example.com"), "{result:?}");
    assert!(!addrs.contains(&"boss@work.test"), "不得跨账户泄露");
    assert!(!addrs.contains(&"hidden.bcc@secret.test"), "不得读取 drafts 表");
    assert!(!addrs.contains(&"bad..addr@x.com"), "非法地址必须被过滤");

    let jane = result
        .contacts
        .iter()
        .find(|c| c.address == "Jane.Doe@example.com")
        .unwrap();
    assert_eq!(jane.name.as_deref(), Some("Doe, Jane"));

    // RpcClient::search_contacts DTO 往返
    let client_res = user
        .search_contacts(&SearchContactsRequest {
            account: "personal",
            query: Some("jane"),
            limit: Some(5),
        })
        .await
        .unwrap();
    assert_eq!(client_res.contacts.len(), 1);
    assert_eq!(client_res.contacts[0].address, "Jane.Doe@example.com");

    // 3. 参数严格校验
    let invalid_cases = [
        json!({}),                                                     // 缺 account
        json!({ "account": "" }),                                      // 空 account
        json!({ "account": "personal", "extra": 1 }),                  // 未知键
        json!({ "account": "personal", "limit": 0 }),                  // limit = 0
        json!({ "account": "personal", "limit": -1 }),                 // 负 limit
        json!({ "account": "personal", "limit": "8" }),                // 非整数 limit
        json!({ "account": "personal", "query": 123 }),                // 非字符串 query
        json!({ "account": "personal", "query": "a\nb" }),             // query 含控制字符
        json!({ "account": "personal", "query": "a".repeat(MAX_CONTACT_QUERY_BYTES + 1) }),
    ];
    for params in invalid_cases {
        let err = user
            .call("searchContacts", params.clone())
            .await
            .unwrap_err();
        assert_eq!(err.rpc_code, Some(-32602), "params={params} err={err:?}");
    }

    // 4. limit > 20 在服务端钳制而不报错，且结果不超过 20
    let clamped = user
        .call("searchContacts", json!({ "account": "personal", "limit": 99 }))
        .await
        .unwrap();
    assert!(clamped["contacts"].as_array().unwrap().len() <= 20);

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}
