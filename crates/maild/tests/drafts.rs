//! M4.5-G03 daemon 拥有的持久草稿验收（ADR-0034 实施切片）。
//!
//! 钉住的性质：写进去的草稿**比写它的那个 daemon 活得久**；过期的更新被拒绝且一个字节都不写；
//! 超限是拒绝而不是截断，被拒之后库里那份原封不动；附件是引用、写入时当场校验、读回时现查
//! 「还在不在」；草稿自成一个授权域——只申请「读邮件」的主体既读不到也写不了。
//!
//! 与 G02 的验收同一姿态：断言的是**协议契约**。前端如何渲染、何时自动保存不在这里。

use std::time::Duration;

use maild::drafts::MAX_DRAFT_BODY_BYTES;
use maild::events::{MailAttachment, MailMessage};
use maild::folders::FolderRole;
use maild::store::{DraftWrite, Store};
use maild::{run_daemon_until, DaemonOptions};
use maild_client::{ClientError, Endpoint, RpcClient};
use maild_paths::AppPaths;
use serde_json::json;

mod common;
use common::fake_mail::FakeMail;

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

/// 草稿要求 `account` 存在（ADR-0034 决策 7 的邻接约定）：配一个指向假服务器的账户，
/// 这样「账户存在」这件事是真的，而不是测试特意绕开的。
fn write_config(paths: &AppPaths, imap_port: u16) {
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
        format!(
            r#"
        [plugins.store.config]
        db_path = "mail.sqlite3"
        [plugins.api.config]
        transport = "unix"
        listen = "mail.sock"
        [plugins.frontend.config]
        default = "builtin-egui"
        [plugins.external.config]

        [[accounts]]
        account = "e2e"
        server = "127.0.0.1"
        port = {imap_port}
        username = "dev"
        password = "dev"
        "#
        ),
    )
    .unwrap();
}

/// schema 2 的前端：`capabilities` 只回答路由，`permissions` 才是授权（ADR-0030 决策 2）。
fn install_frontend(paths: &AppPaths, id: &str, routes: &str, permissions: &str) {
    let package = paths.frontend_dir.join(id);
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("frontend.toml"),
        format!(
            "schema_version = 2\nprotocol_version = 1\nid = \"{id}\"\nname = \"Draft fixture\"\n\
             version = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [{routes}]\n\
             permissions = [{permissions}]\n"
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

/// 另一个账户的一封旧信：草稿要能引用它的附件（跨账户转发是合法的，决策 6）。
/// 这个账户不在配置里，daemon 不会同步它，因此 `stored = false` 的那个 part 稳定存在。
fn seed_archive(paths: &AppPaths) {
    let db = paths.data_dir.join("mail.sqlite3");
    let store = Store::open(db.to_str().unwrap()).unwrap();
    store
        .upsert_folder("archive", "INBOX", "收件箱", Some(FolderRole::Inbox))
        .unwrap();
    store
        .insert_message(&MailMessage {
            account: "archive".into(),
            uid: "77".into(),
            sender: "编辑部 <editor@shanhai.example>".into(),
            recipient: "me@example.test".into(),
            subject: "十月刊校样".into(),
            body: "附件是校样。".into(),
            received_at: 1_757_600_000,
            folder: "INBOX".into(),
            has_attachments: true,
            attachments: vec![
                MailAttachment {
                    part_index: 2,
                    filename: "校样-十月.pdf".into(),
                    mime_type: "application/pdf".into(),
                    size: 11,
                    stored: true,
                    content: Some(b"proof-bytes".to_vec()),
                    ..Default::default()
                },
                MailAttachment {
                    part_index: 4,
                    filename: "原始扫描.tiff".into(),
                    mime_type: "image/tiff".into(),
                    size: 40 * 1024 * 1024,
                    stored: false,
                    content: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        })
        .unwrap();
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

async fn frontend_connection(client: &RpcClient, id: &str) -> maild_client::StreamConnection {
    let token = client
        .reveal_plugin_token(id, Some("frontend"))
        .await
        .expect("受管前端有身份条目");
    let mut connection = client.connect_stream().await.unwrap();
    connection.authenticate(&token).await.expect("认证成功");
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

/// 一套完整的假服务器 + daemon + 一个持读写两词的前端连接。
struct Harness {
    _server: FakeMail,
    root: tempfile::TempDir,
    paths: AppPaths,
    task: DaemonTask,
    shutdown: tokio::sync::oneshot::Sender<()>,
    client: RpcClient,
}

async fn start(extra_frontends: &[(&str, &str, &str)]) -> Harness {
    use_memory_credential_store();
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    let root = tempfile::tempdir().unwrap();
    let paths = test_paths(root.path());
    write_config(&paths, server.imap_addr.port());
    install_frontend(&paths, "composer.fixture", "\"compose\"", "\"drafts_read\", \"drafts_write\"");
    for (id, routes, permissions) in extra_frontends {
        install_frontend(&paths, id, routes, permissions);
    }
    seed_archive(&paths);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let client = unix_client(&socket);
    Harness {
        _server: server,
        root,
        paths,
        task,
        shutdown,
        client,
    }
}

impl Harness {
    async fn composer(&self) -> maild_client::StreamConnection {
        frontend_connection(&self.client, "composer.fixture").await
    }

    /// 关掉 daemon 并把数据库还给调用方：这是「比写它的进程活得久」唯一诚实的验法。
    async fn stop_and_reopen(self) -> (Store, tempfile::TempDir) {
        let db = self.paths.data_dir.join("mail.sqlite3");
        stop_daemon(self.task, self.shutdown).await;
        let store = Store::open(db.to_str().unwrap()).expect("daemon 退出后数据库可重开");
        (store, self.root)
    }

    async fn stop(self) {
        stop_daemon(self.task, self.shutdown).await;
        drop(self.root);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_draft_outlives_the_daemon_that_wrote_it() {
    let harness = start(&[]).await;
    let mut composer = harness.composer().await;

    let created = call_on(
        &mut composer,
        "createDraft",
        json!({
            "account": "e2e",
            "to": ["alice@example.test"],
            "subject": "十月共读：报名与选书",
            "body": "各位好：\n这轮我们读《夜航西飞》。",
        }),
    )
    .await
    .expect("新建草稿")["draft"]
        .clone();
    let id = created["id"].as_str().expect("草稿有 id").to_owned();
    assert!(id.starts_with("draft_"), "id 形状: {id}");
    assert_eq!(created["version"], 1);
    assert_eq!(created["to"], json!(["alice@example.test"]));

    // 补一次丁：缺席的字段不动，出现的字段整体替换（决策 4）。
    let updated = call_on(
        &mut composer,
        "updateDraft",
        json!({ "id": id, "base_version": 1, "cc": ["bob@example.test"] }),
    )
    .await
    .expect("更新草稿")["draft"]
        .clone();
    assert_eq!(updated["version"], 2);
    assert_eq!(updated["subject"], "十月共读：报名与选书");
    assert_eq!(updated["cc"], json!(["bob@example.test"]));
    assert!(
        updated["updated_at"].as_u64().unwrap() >= created["created_at"].as_u64().unwrap(),
        "回执带的是提交之后的时刻"
    );

    // 列表里是摘要，不含正文。
    let listed = call_on(&mut composer, "listDrafts", json!({ "account": "e2e" }))
        .await
        .expect("列出草稿");
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["drafts"][0]["id"], json!(id));
    assert!(listed["drafts"][0].get("body").is_none(), "摘要不带正文");
    assert_eq!(listed["drafts"][0]["snippet"], "各位好： 这轮我们读《夜航西飞》。");

    // daemon 没了，内容还在——这就是「输入中崩溃/重启可恢复」。
    let (store, _root) = harness.stop_and_reopen().await;
    let recovered = store.get_draft(&id).expect("可读").expect("草稿还在");
    assert_eq!(recovered.version, 2);
    assert_eq!(recovered.subject, "十月共读：报名与选书");
    assert_eq!(recovered.body, "各位好：\n这轮我们读《夜航西飞》。");
    assert_eq!(recovered.cc, vec!["bob@example.test".to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn a_stale_update_is_refused_and_writes_nothing() {
    let harness = start(&[]).await;
    let mut first = harness.composer().await;
    let mut second = harness.composer().await;

    let id = call_on(
        &mut first,
        "createDraft",
        json!({ "account": "e2e", "body": "第一版" }),
    )
    .await
    .expect("新建")["draft"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // 两个编辑者都读到了 v1。先到的赢。
    call_on(
        &mut first,
        "updateDraft",
        json!({ "id": id, "base_version": 1, "body": "第一位编辑者写的" }),
    )
    .await
    .expect("第一次更新成功");

    let stale = call_on(
        &mut second,
        "updateDraft",
        json!({ "id": id, "base_version": 1, "body": "第二位编辑者写的" }),
    )
    .await
    .expect_err("过期更新必须被拒绝");
    assert_eq!(stale.rpc_code, Some(-32006));
    // 回执要能据以行动：当前版本号在 `data` 里，调用方重取一次就能继续。
    assert_eq!(stale.rpc_data.expect("带 data")["version"], 2);

    // 被拒绝的那次一个字节都没写。
    let current = call_on(&mut first, "getDraft", json!({ "id": id }))
        .await
        .expect("可读")["draft"]
        .clone();
    assert_eq!(current["body"], "第一位编辑者写的");
    assert_eq!(current["version"], 2);

    // 重取之后带对版本号就能写进去——冲突是「你读到的那份还在不在」，不是永久拒绝。
    call_on(
        &mut second,
        "updateDraft",
        json!({ "id": id, "base_version": 2, "body": "看过之后再写" }),
    )
    .await
    .expect("带上当前版本即可写入");
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_content_is_refused_and_the_saved_version_is_untouched() {
    let harness = start(&[]).await;
    let mut composer = harness.composer().await;

    let id = call_on(
        &mut composer,
        "createDraft",
        json!({ "account": "e2e", "body": "还没写完" }),
    )
    .await
    .expect("新建")["draft"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let refused = call_on(
        &mut composer,
        "updateDraft",
        json!({ "id": id, "base_version": 1, "body": "a".repeat(MAX_DRAFT_BODY_BYTES + 1) }),
    )
    .await
    .expect_err("超限正文必须被拒绝");
    assert_eq!(refused.rpc_code, Some(-32007));
    let data = refused.rpc_data.expect("带 data");
    assert_eq!(data["field"], "body");
    assert_eq!(data["limit"], MAX_DRAFT_BODY_BYTES);

    // 头注入在最早一站被拦下，而且是参数错误而不是「太大」。
    assert_eq!(
        code_of(
            call_on(
                &mut composer,
                "updateDraft",
                json!({
                    "id": id, "base_version": 1,
                    "to": ["a@b.example\r\nBcc: evil@x.example"],
                }),
            )
            .await,
            "带换行的收件人"
        ),
        -32602
    );

    // 两次拒绝之后，库里那份原封不动——版本号都没动。
    let current = call_on(&mut composer, "getDraft", json!({ "id": id }))
        .await
        .expect("可读")["draft"]
        .clone();
    assert_eq!(current["body"], "还没写完");
    assert_eq!(current["version"], 1);
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn attachments_are_references_that_must_resolve_when_written() {
    let harness = start(&[]).await;
    let mut composer = harness.composer().await;

    let id = call_on(
        &mut composer,
        "createDraft",
        json!({ "account": "e2e", "subject": "转发：十月刊校样" }),
    )
    .await
    .expect("新建")["draft"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // 跨账户引用：草稿属于 e2e，附件来自 archive（决策 6）。
    let with_attachment = call_on(
        &mut composer,
        "updateDraft",
        json!({
            "id": id, "base_version": 1,
            "attachments": [{
                "account": "archive", "folder": "INBOX", "uid": "77", "part_index": 2
            }],
        }),
    )
    .await
    .expect("引用一个已落库的 part")["draft"]
        .clone();
    let reference = &with_attachment["attachments"][0];
    assert_eq!(reference["filename"], "校样-十月.pdf");
    assert_eq!(reference["mime_type"], "application/pdf");
    assert_eq!(reference["size"], 11);
    assert_eq!(reference["stored"], true);

    // 三种「拿不到」各自有码，与 readAttachment 同义。
    for (bad, expected, what) in [
        (
            json!({ "account": "archive", "folder": "Sent", "uid": "77", "part_index": 2 }),
            -32002,
            "文件夹不匹配",
        ),
        (
            json!({ "account": "e2e", "folder": "INBOX", "uid": "77", "part_index": 2 }),
            -32002,
            "账户不匹配",
        ),
        (
            json!({ "account": "archive", "folder": "INBOX", "uid": "77", "part_index": 9 }),
            -32003,
            "part 不存在",
        ),
        (
            json!({ "account": "archive", "folder": "INBOX", "uid": "77", "part_index": 4 }),
            -32004,
            "字节未落库",
        ),
    ] {
        assert_eq!(
            code_of(
                call_on(
                    &mut composer,
                    "updateDraft",
                    json!({ "id": id, "base_version": 2, "attachments": [bad] }),
                )
                .await,
                what,
            ),
            expected,
            "{what} 应当被拒绝"
        );
    }

    // 四次拒绝之后引用列表没有被清空——整次写入回滚，不是「先删后失败」。
    let current = call_on(&mut composer, "getDraft", json!({ "id": id }))
        .await
        .expect("可读")["draft"]
        .clone();
    assert_eq!(current["version"], 2);
    assert_eq!(current["attachments"].as_array().unwrap().len(), 1);
    assert_eq!(current["attachments"][0]["uid"], "77");
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_reference_whose_source_disappeared_reads_back_as_not_stored() {
    let harness = start(&[]).await;
    let mut composer = harness.composer().await;
    let id = call_on(
        &mut composer,
        "createDraft",
        json!({
            "account": "e2e",
            "attachments": [{
                "account": "archive", "folder": "INBOX", "uid": "77", "part_index": 2
            }],
        }),
    )
    .await
    .expect("新建带引用的草稿")["draft"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // 源邮件被删掉之后，引用还在但 `stored` 变成 false：UI 能提前把话说在前面，
    // 而不是等到发信才失败（决策 6）。
    let (store, _root) = harness.stop_and_reopen().await;
    // UIDVALIDITY 变了 → 整个文件夹重建：源邮件与它的附件一起消失。
    let folder = store
        .folder_by_name("archive", "INBOX")
        .unwrap()
        .expect("预置的文件夹");
    store.rebuild_folder(folder.id, 999).expect("重建文件夹");
    let draft = store.get_draft(&id).unwrap().expect("草稿还在");
    assert_eq!(draft.attachments.len(), 1);
    assert_eq!(draft.attachments[0].filename, "校样-十月.pdf");
    assert!(!draft.attachments[0].stored, "源 part 已不可读");
}

#[tokio::test(flavor = "current_thread")]
async fn reading_a_draft_and_writing_one_are_two_different_words() {
    let harness = start(&[
        ("reader.fixture", "\"message\"", "\"drafts_read\""),
        ("writer.fixture", "\"compose\"", "\"drafts_write\""),
        ("mailonly.fixture", "\"inbox\", \"message\"", ""),
    ])
    .await;
    let mut composer = harness.composer().await;
    let id = call_on(
        &mut composer,
        "createDraft",
        json!({ "account": "e2e", "subject": "授权验收" }),
    )
    .await
    .expect("新建")["draft"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // 只持 drafts_read：读得到，写不了。
    let mut reader = frontend_connection(&harness.client, "reader.fixture").await;
    assert_eq!(
        call_on(&mut reader, "getDraft", json!({ "id": id }))
            .await
            .expect("可读")["draft"]["subject"],
        "授权验收"
    );
    assert_eq!(
        code_of(
            call_on(
                &mut reader,
                "updateDraft",
                json!({ "id": id, "base_version": 1, "subject": "越权" }),
            )
            .await,
            "只读主体改草稿",
        ),
        -32016
    );

    // 只持 drafts_write：写得了，读不到。
    let mut writer = frontend_connection(&harness.client, "writer.fixture").await;
    call_on(
        &mut writer,
        "updateDraft",
        json!({ "id": id, "base_version": 1, "subject": "写入方改的" }),
    )
    .await
    .expect("只写主体可以写");
    assert_eq!(
        code_of(
            call_on(&mut writer, "getDraft", json!({ "id": id })).await,
            "只写主体读草稿",
        ),
        -32016
    );

    // `messages_read` 不含草稿：`compose` 路由也不隐含 drafts_write（ADR-0030 决策 2）。
    let mut mail_only = frontend_connection(&harness.client, "mailonly.fixture").await;
    for (method, params) in [
        ("listDrafts", json!({})),
        ("getDraft", json!({ "id": id })),
        (
            "updateDraft",
            json!({ "id": id, "base_version": 2, "subject": "越权" }),
        ),
        ("deleteDraft", json!({ "id": id })),
    ] {
        assert_eq!(
            code_of(
                call_on(&mut mail_only, method, params).await,
                &format!("只读邮件的主体调用 {method}"),
            ),
            -32016,
            "{method} 不该对只持 messages_read 的主体开放"
        );
    }
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn discarding_a_draft_can_be_made_conditional() {
    let harness = start(&[]).await;
    let mut composer = harness.composer().await;
    let id = call_on(
        &mut composer,
        "createDraft",
        json!({ "account": "e2e", "subject": "待丢弃" }),
    )
    .await
    .expect("新建")["draft"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call_on(
        &mut composer,
        "updateDraft",
        json!({ "id": id, "base_version": 1, "subject": "别处改过了" }),
    )
    .await
    .expect("更新");

    // 带着过期版本号的丢弃被拒绝：好让 UI 先问一句。
    assert_eq!(
        code_of(
            call_on(
                &mut composer,
                "deleteDraft",
                json!({ "id": id, "base_version": 1 }),
            )
            .await,
            "过期丢弃",
        ),
        -32006
    );
    assert_eq!(
        call_on(&mut composer, "listDrafts", json!({}))
            .await
            .expect("可列")["total"],
        1
    );

    let deleted = call_on(
        &mut composer,
        "deleteDraft",
        json!({ "id": id, "base_version": 2 }),
    )
    .await
    .expect("带上当前版本即可丢弃");
    assert_eq!(deleted["deleted"], true);
    assert_eq!(deleted["draft"]["subject"], "别处改过了");
    assert_eq!(
        code_of(
            call_on(&mut composer, "getDraft", json!({ "id": id })).await,
            "读已丢弃的草稿",
        ),
        -32005
    );
    assert_eq!(
        call_on(&mut composer, "listDrafts", json!({}))
            .await
            .expect("可列")["total"],
        0
    );
    harness.stop().await;
}

/// 上限到顶时**拒绝新建**，不淘汰最旧的一份（决策 5）：用户写了一半的信不该被我们替他删掉。
#[test]
fn the_per_account_limit_refuses_the_next_draft_instead_of_evicting_the_oldest() {
    use maild::drafts::{DraftPatch, MAX_DRAFTS_PER_ACCOUNT};

    let store = Store::open("").expect("内存库");
    let first = maild::drafts::generate_draft_id();
    let patch = DraftPatch {
        body: Some("第一份".into()),
        ..DraftPatch::default()
    };
    assert!(matches!(
        store.create_draft(&first, "solo", &patch, 1).unwrap(),
        DraftWrite::Saved(_)
    ));
    for index in 1..MAX_DRAFTS_PER_ACCOUNT {
        let id = maild::drafts::generate_draft_id();
        assert!(
            matches!(
                store
                    .create_draft(&id, "solo", &DraftPatch::default(), index as u64)
                    .unwrap(),
                DraftWrite::Saved(_)
            ),
            "第 {index} 份应当写得进去"
        );
    }
    assert!(matches!(
        store
            .create_draft(
                &maild::drafts::generate_draft_id(),
                "solo",
                &DraftPatch::default(),
                9_999,
            )
            .unwrap(),
        DraftWrite::LimitReached { .. }
    ));
    // 最旧的那份没被挤掉。
    assert_eq!(
        store.get_draft(&first).unwrap().expect("还在").body,
        "第一份"
    );
    // 另一个账户不受影响：上限是按账户算的。
    assert!(matches!(
        store
            .create_draft(
                &maild::drafts::generate_draft_id(),
                "other",
                &DraftPatch::default(),
                9_999,
            )
            .unwrap(),
        DraftWrite::Saved(_)
    ));
}

/// G04: upload IDs do not bypass principal binding; send-only cannot read draft bytes.
#[tokio::test(flavor = "current_thread")]
async fn attachment_upload_authorization_and_subject_ownership() {
    let harness = start(&[
        ("other.fixture", "\"compose\"", "\"drafts_write\""),
        ("reader.fixture", "\"message\"", "\"drafts_read\""),
        ("sender.fixture", "\"compose\"", ""),
    ])
    .await;
    let mut composer = harness.composer().await;
    let mut other = frontend_connection(&harness.client, "other.fixture").await;
    let mut reader = frontend_connection(&harness.client, "reader.fixture").await;
    let mut sender = frontend_connection(&harness.client, "sender.fixture").await;
    let d = call_on(&mut composer, "createDraft", json!({"account":"e2e"}))
        .await
        .unwrap()["draft"]
        .clone();
    let params =
        json!({"draft_id":d["id"],"filename":"empty.txt","mime_type":"text/plain","size":0});
    assert_eq!(
        code_of(
            call_on(&mut reader, "beginAttachmentUpload", params.clone()).await,
            "read-only upload"
        ),
        -32016
    );
    let u = call_on(&mut composer, "beginAttachmentUpload", params)
        .await
        .unwrap()["upload"]
        .clone();
    for (method, p) in [
        (
            "writeAttachmentUpload",
            json!({"upload_id":u["upload_id"],"offset":0,"data":"YQ=="}),
        ),
        (
            "finishAttachmentUpload",
            json!({"upload_id":u["upload_id"],"base_version":1}),
        ),
        (
            "cancelAttachmentUpload",
            json!({"upload_id":u["upload_id"]}),
        ),
    ] {
        assert_eq!(
            code_of(
                call_on(&mut other, method, p.clone()).await,
                "other principal"
            ),
            -32008
        );
        assert_eq!(
            code_of(call_on(&mut reader, method, p).await, "no drafts_write"),
            -32016
        );
    }
    call_on(
        &mut composer,
        "finishAttachmentUpload",
        json!({"upload_id":u["upload_id"],"base_version":1}),
    )
    .await
    .unwrap();
    assert_eq!(code_of(call_on(&mut sender,"sendFromAccount",json!({"account":"e2e","to":"x@example.test","attachments":[{"draft_id":d["id"],"upload_id":u["upload_id"]}]})).await,"send-only draft read"),-32016);
    assert_eq!(
        code_of(
            call_on(
                &mut other,
                "finishAttachmentUpload",
                json!({"upload_id":u["upload_id"],"base_version":1})
            )
            .await,
            "completed foreign upload"
        ),
        -32008
    );
    harness.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn send_draft_requires_all_three_authorities_and_definite_failure_keeps_content() {
    let harness = start(&[
        ("readonly.sender", "\"compose\"", "\"drafts_read\""),
        (
            "writer.only",
            "\"message\"",
            "\"drafts_read\",\"drafts_write\"",
        ),
    ])
    .await;
    let mut composer = harness.composer().await;
    let mut readonly = frontend_connection(&harness.client, "readonly.sender").await;
    let mut writer = frontend_connection(&harness.client, "writer.only").await;
    let d = call_on(
        &mut composer,
        "createDraft",
        json!({"account":"e2e","to":["a@example.test"],"body":"must survive"}),
    )
    .await
    .unwrap()["draft"]
        .clone();
    for connection in [&mut readonly, &mut writer] {
        assert_eq!(
            code_of(
                call_on(
                    connection,
                    "sendDraft",
                    json!({"id":d["id"],"base_version":1})
                )
                .await,
                "missing authority"
            ),
            -32016
        );
    }
    // This fixture account has no SMTP profile: rejection precedes any transport delivery.
    let sent = call_on(
        &mut composer,
        "sendDraft",
        json!({"id":d["id"],"base_version":1}),
    )
    .await
    .unwrap()["draft"]
        .clone();
    assert_eq!(sent["send_state"], "failed");
    assert_eq!(sent["body"], "must survive");
    assert_eq!(
        code_of(
            call_on(
                &mut composer,
                "sendDraft",
                json!({"id":d["id"],"base_version":1})
            )
            .await,
            "stale send"
        ),
        -32006
    );
    harness.stop().await;
}
