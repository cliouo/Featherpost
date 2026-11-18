//! M4.5-03 桥的 hermetic 验收（ADR-0031 决策 3/4）：真 daemon + 预置 SQLite，无窗口。
//!
//! 钉住的性质：桥以 `frontend:<FRONTEND_ID>`（`maild_brand::BUNDLED_FRONTEND_ID`）主体运行而不是 `user`；投影只给 UI 该看的；
//! 四类错误码；包未安装时不降级、不静默。

use std::time::Duration;

use maild::events::{MailAttachment, MailMessage};
use maild::folders::FolderRole;
use maild::store::Store;
use maild::{run_daemon_until, DaemonOptions};
use maild_client::{Endpoint, OpenIntent, RpcClient};
use maild_paths::AppPaths;
use maild_ui::bridge::{UiCompat, UiErrorCode};
use maild_ui::documents::{route, DocumentStore, ProtocolBase, Route};
use maild_ui::{Bridge, FRONTEND_ID};

/// 桥认证后的主体名：`frontend:` + 品牌前端 id，对任意 `brand.toml` 成立。
fn frontend_principal() -> String {
    format!("frontend:{FRONTEND_ID}")
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

/// 把仓库里的 `package/frontend.toml` 原样装进受管目录——测试跑的就是要发布的那份声明。
fn install_frontend_package(paths: &AppPaths) {
    let package = paths.frontend_dir.join(FRONTEND_ID);
    std::fs::create_dir_all(&package).unwrap();
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("package/frontend.toml"),
    )
    .unwrap();
    std::fs::write(package.join("frontend.toml"), manifest).unwrap();
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

fn seeded(account: &str, folder: &str, uid: &str, subject: &str, when: u64) -> MailMessage {
    MailMessage {
        account: account.into(),
        uid: uid.into(),
        sender: "刊物编辑 <editor@example.test>".into(),
        recipient: "me@example.test".into(),
        subject: subject.into(),
        body: format!("{subject} 的正文"),
        received_at: when,
        date_at: Some(when - 60),
        folder: folder.into(),
        seen: true,
        ..Default::default()
    }
}

/// 在 daemon 打开数据库之前预置：两个文件夹、三封收件、一封归档。`Store` 持有数据库锁，
/// 必须在 daemon 启动前 drop。
fn seed_store(paths: &AppPaths) {
    let db = paths.data_dir.join("mail.sqlite3");
    let store = Store::open(db.to_str().unwrap()).unwrap();
    store
        .upsert_folder("demo", "INBOX", "收件箱", Some(FolderRole::Inbox))
        .unwrap();
    store
        .upsert_folder("demo", "Archive", "归档", Some(FolderRole::Archive))
        .unwrap();
    let mut oldest = seeded("demo", "INBOX", "1", "最早的一封", 1_757_000_000);
    oldest.seen = false;
    let mut html = seeded("demo", "INBOX", "2", "带 HTML 与附件", 1_757_100_000);
    html.body_html = "<html><head><script>alert(1)</script></head><body><p>版式</p>\
        <img src=\"cid:logo@demo\"><img src=\"https://remote.example/t.gif\">\
        <a href=\"https://shop.example/x\">链接</a></body></html>"
        .into();
    html.has_attachments = true;
    html.attachments = vec![
        MailAttachment {
            part_index: 2,
            filename: "印刷确认.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 2048,
            stored: true,
            content: Some(vec![0u8; 2048]),
            ..Default::default()
        },
        MailAttachment {
            part_index: 3,
            filename: "logo.png".into(),
            mime_type: "image/png".into(),
            content_id: "logo@demo".into(),
            inline: true,
            size: 4,
            stored: true,
            content: Some(b"png!".to_vec()),
        },
        MailAttachment {
            part_index: 4,
            filename: "big.mov".into(),
            mime_type: "video/quicktime".into(),
            size: 10 * 1024 * 1024,
            stored: false,
            content: None,
            ..Default::default()
        },
    ];
    let newest = seeded("demo", "INBOX", "3", "最新的一封", 1_757_200_000);
    let archived = seeded("demo", "Archive", "1", "去年的归档", 1_720_000_000);
    for message in [oldest, html, newest, archived] {
        assert!(store.insert_message(&message).unwrap());
    }
}

type DaemonTask = tokio::task::JoinHandle<Result<(), maild::DaemonError>>;

fn management_edit(value: serde_json::Value) -> maild_ui::settings::AccountEdit {
    serde_json::from_value(value).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn settings_accounts_rules_and_addresses_use_authenticated_management_contracts() {
    use maild_ui::settings::{AccountAction, AddressAction};
    use serde_json::json;
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");
    let server = fake_mail::FakeMail::start_imap_implicit_and_smtp_starttls().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    use std::io::Write;
    std::fs::OpenOptions::new().append(true).open(&paths.config_file).unwrap().write_all(b"\n[plugins.rules.config]\n").unwrap();
    install_frontend_package(&paths);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let bridge = Bridge::spawn(Endpoint::Unix{path:socket}).unwrap();
    wait_for(&bridge, |s|s.phase=="online").await;
    assert!(bridge.settings_profiles().await.unwrap().is_empty());
    let draft = json!({"display_name":"验收邮箱","address":"settings@example.test",
        "incoming":{"kind":"imap","server":"127.0.0.1","port":server.imap_addr.port(),"security":"implicit","username":"settings@example.test","mailbox":"INBOX","idle":true,"poll_secs":300},
        "outgoing":{"kind":"smtp","server":"127.0.0.1","port":server.smtp_addr.port(),"security":"starttls","username":"settings@example.test","credential":"incoming"}});
    let request = json!({"id":null,"revision":null,"draft":draft,"secrets":{"incoming":"unique-secret-not-for-webview"}});
    let probe=bridge.settings_probe(management_edit(request.clone())).await.unwrap();
    assert!(probe.incoming.ok && probe.outgoing.unwrap().ok);
    assert!(bridge.settings_profiles().await.unwrap().is_empty(),"probe does not save an account");
    let first=bridge.settings_save_account(management_edit(request)).await.unwrap();
    let rendered=serde_json::to_string(&first).unwrap();
    assert!(!rendered.contains("unique-secret-not-for-webview"));
    assert_eq!(first.credentials.incoming,"saved");
    let existing_probe=json!({"id":first.id,"revision":first.revision,"draft":draft,"secrets":{"incoming":"unique-secret-not-for-webview"}});
    assert!(bridge.settings_probe(management_edit(existing_probe.clone())).await.unwrap().incoming.ok);
    let mut stale_probe=existing_probe;stale_probe["revision"]=json!(first.revision+1);
    assert_eq!(bridge.settings_probe(management_edit(stale_probe)).await.unwrap_err().code,UiErrorCode::Conflict);
    let mut patch=draft.clone();patch["display_name"]=json!("已修改名称");
    let update=json!({"id":first.id,"revision":first.revision,"draft":patch,"secrets":{}});
    let next=bridge.settings_save_account(management_edit(update.clone())).await.unwrap();
    assert_eq!(next.display_name,"已修改名称");
    assert_eq!(bridge.settings_save_account(management_edit(update)).await.unwrap_err().code,UiErrorCode::Conflict);
    let mut rules=bridge.settings_rules(None).await.unwrap().rules;
    rules.show_sensitive=true;rules.silent_keywords=vec!["新闻".into()];
    let saved=bridge.settings_rules(Some(rules.clone())).await.unwrap();
    assert_eq!(saved.persisted,Some(false));assert_eq!(saved.rules,rules);
    assert_eq!(bridge.settings_rules(None).await.unwrap().rules,rules);
    bridge.settings_address_action(AddressAction::Create{account:first.id.clone(),kind:"alias".into(),address:Some("alias@example.test".into()),label:"客服".into(),expires_secs:None}).await.unwrap();
    let addresses=bridge.settings_addresses(&first.id,0).await.unwrap();
    assert_eq!(addresses.total,1);
    let address=&addresses.addresses[0];
    bridge.settings_address_action(AddressAction::Update{id:address.id.clone(),state:Some("disabled".into()),label:None}).await.unwrap();
    assert_eq!(bridge.settings_addresses(&first.id,0).await.unwrap().addresses[0].state,"disabled");
    bridge.settings_address_action(AddressAction::Remove{id:address.id.clone()}).await.unwrap();
    assert_eq!(bridge.settings_addresses(&first.id,0).await.unwrap().total,0);
    let mut no_outgoing=draft.clone();no_outgoing["outgoing"]=serde_json::Value::Null;
    let next=bridge.settings_save_account(management_edit(json!({"id":first.id,"revision":next.revision,"draft":no_outgoing,"secrets":{}}))).await.unwrap();
    assert!(next.outgoing.is_none(),"unchecking sending must remove the outgoing endpoint");
    bridge.settings_account_action(AccountAction::Enable{id:first.id.clone(),revision:next.revision,enabled:false}).await.unwrap();
    let disabled=bridge.settings_profiles().await.unwrap().remove(0);
    assert!(!disabled.enabled);
    bridge.settings_account_action(AccountAction::Remove{id:first.id,revision:disabled.revision}).await.unwrap();
    assert!(bridge.settings_profiles().await.unwrap().is_empty());
    bridge.shutdown().await;let _=shutdown.send(());task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn settings_package_snapshot_upgrade_rollback_and_default_persist() {
    use maild_ui::{settings::PackageAction,settings_packages::PackageSelections};
    let directory=tempfile::tempdir().unwrap();let paths=test_paths(directory.path());
    write_config(&paths);install_frontend_package(&paths);
    let former=paths.frontend_dir.join("former.frontend");std::fs::create_dir(&former).unwrap();
    std::fs::write(former.join("frontend.toml"),std::fs::read_to_string(paths.frontend_dir.join(FRONTEND_ID).join("frontend.toml")).unwrap().replace(FRONTEND_ID,"former.frontend")).unwrap();
    let config=std::fs::read_to_string(&paths.config_file).unwrap().replace("default = \"builtin-egui\"","default = \"former.frontend\"");
    std::fs::write(&paths.config_file,config).unwrap();
    let (mut task,shutdown)=spawn_daemon(&paths);let socket=paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task,&socket).await;
    let bridge=Bridge::spawn(Endpoint::Unix{path:socket.clone()}).unwrap();wait_for(&bridge,|s|s.phase=="online").await;
    let source=directory.path().join("candidate");std::fs::create_dir(&source).unwrap();
    let manifest=|version:&str|format!("schema_version=1\nprotocol_version=1\nid=\"test.settings\"\nname=\"Settings fixture\"\nversion=\"{version}\"\nlifecycle=\"independent\"\ncapabilities=[\"messages_read\"]\n");
    std::fs::write(source.join("plugin.toml"),manifest("1.0.0")).unwrap();
    let selections=PackageSelections::default();
    let selected=selections.select(&bridge,&source,"plugin").await.unwrap();
    assert_eq!(selected.preview.capabilities,vec!["messages_read"]);
    assert!(bridge.settings_packages().await.unwrap().plugins.is_empty());
    // Changes to the source after approval do not replace the staged snapshot.
    std::fs::write(source.join("plugin.toml"),manifest("2.0.0")).unwrap();
    assert_eq!(selections.install(&bridge,&selected.token).await.unwrap().version,"1.0.0");
    assert!(selections.install(&bridge,&selected.token).await.is_err());
    let selected=selections.select(&bridge,&source,"plugin").await.unwrap();
    assert_eq!(selections.install(&bridge,&selected.token).await.unwrap().action,"upgraded");
    let rollback=bridge.settings_rollback_preview("plugin","test.settings").await.unwrap();
    assert_eq!(rollback.version,"1.0.0");
    let mut forged=rollback.clone();forged.capabilities.clear();
    assert!(bridge.settings_rollback(forged).await.is_err());
    bridge.settings_rollback(rollback).await.unwrap();
    assert_eq!(bridge.settings_packages().await.unwrap().plugins[0].version,"1.0.0");
    bridge.settings_package_action(PackageAction::Enable{id:"test.settings".into(),enabled:true}).await.unwrap();
    bridge.shutdown().await;let _=shutdown.send(());task.await.unwrap().unwrap();
    let (mut task,shutdown)=spawn_daemon(&paths);wait_socket_or_fail(&mut task,&socket).await;
    let bridge=Bridge::spawn(Endpoint::Unix{path:socket.clone()}).unwrap();wait_for(&bridge,|s|s.phase=="online").await;
    assert!(bridge.settings_packages().await.unwrap().plugins[0].enabled,"independent enabled intent survives restart");
    bridge.settings_package_action(PackageAction::Remove{kind:"plugin".into(),id:"test.settings".into()}).await.unwrap();
    assert!(bridge.settings_packages().await.unwrap().plugins.is_empty());
    let selected=selections.select(&bridge,&source,"plugin").await.unwrap();selections.cancel(&selected.token);
    assert!(selections.install(&bridge,&selected.token).await.is_err());
    assert_eq!(std::fs::read_dir(paths.plugin_dir.join(".incoming")).unwrap().count(),0);
    bridge.settings_package_action(PackageAction::Default{id:FRONTEND_ID.into()}).await.unwrap();
    assert_eq!(bridge.settings_packages().await.unwrap().default,FRONTEND_ID);
    bridge.settings_package_action(PackageAction::Remove{kind:"frontend".into(),id:"former.frontend".into()}).await.unwrap();
    assert!(bridge.raw("removePlugin",serde_json::json!({"kind":"frontend","id":FRONTEND_ID})).await.is_err());
    bridge.shutdown().await;let _=shutdown.send(());task.await.unwrap().unwrap();
    let (mut task,shutdown)=spawn_daemon(&paths);wait_socket_or_fail(&mut task,&socket).await;
    let client=RpcClient::new(Endpoint::Unix{path:socket}).unwrap();
    assert_eq!(client.list_frontends().await.unwrap().default,FRONTEND_ID);
    let _=shutdown.send(());task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn management_methods_deny_a_read_only_frontend_before_side_effects() {
    let directory=tempfile::tempdir().unwrap();let paths=test_paths(directory.path());
    write_config(&paths);install_frontend_package(&paths);
    std::fs::write(paths.frontend_dir.join(FRONTEND_ID).join("frontend.toml"),format!("schema_version=2\nprotocol_version=1\nid=\"{FRONTEND_ID}\"\nname=\"Readonly\"\nversion=\"0.1.0\"\nlifecycle=\"independent\"\ncapabilities=[\"inbox\"]\npermissions=[]\n")).unwrap();
    let (mut task,shutdown)=spawn_daemon(&paths);let socket=paths.runtime_dir.join("mail.sock");wait_socket_or_fail(&mut task,&socket).await;
    let bridge=Bridge::spawn(Endpoint::Unix{path:socket}).unwrap();wait_for(&bridge,|s|s.phase=="online").await;
    for method in ["listAccountProfiles","getPluginInstallRoot","previewPluginPackage","setDefaultFrontend","commitPluginInstall","rollbackPlugin","setSettings","listAddresses","repairBuiltinFrontend","stopManagedAgent"] {
        assert_eq!(bridge.raw(method,serde_json::json!({})).await.unwrap_err().rpc_code,Some(-32016),"{method}");
    }
    assert!(!paths.frontend_dir.join(".default.json").exists());
    assert_eq!(bridge.settings_profiles().await.unwrap_err().code,UiErrorCode::Denied);
    // The only recovery action exposed to JS is opening the native window. A bare test daemon
    // cannot activate an installed app, which is an explicit UI error rather than an auth bypass.
    assert_eq!(bridge.settings_package_action(maild_ui::settings::PackageAction::Recovery).await.unwrap_err().code,UiErrorCode::Invalid);
    bridge.shutdown().await;let _=shutdown.send(());task.await.unwrap().unwrap();
}

fn spawn_daemon(paths: &AppPaths) -> (DaemonTask, tokio::sync::oneshot::Sender<()>) {
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
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

async fn wait_for(bridge: &Bridge, mut predicate: impl FnMut(&maild_ui::UiSnapshot) -> bool) {
    let mut state = bridge.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if predicate(&bridge.snapshot()) {
                return;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("快照未在 10s 内到达预期: {:?}", bridge.snapshot()));
}

#[tokio::test(flavor = "current_thread")]
async fn the_shell_binds_as_its_frontend_and_projects_the_mailbox() {
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    seed_store(&paths);
    install_frontend_package(&paths);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;

    let bridge = Bridge::spawn(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    wait_for(&bridge, |snapshot| snapshot.phase == "online").await;
    let snapshot = bridge.snapshot();
    assert_eq!(snapshot.principal, Some(frontend_principal()));
    assert_eq!(snapshot.identity_failure, None);
    assert_eq!(snapshot.frontend_id, FRONTEND_ID);

    // ADR-0016 握手以前端主体完成：同代后台判兼容，同一 epoch 不重复问。
    assert_eq!(snapshot.compat, UiCompat::Unknown);
    assert!(bridge.check_compat().await, "首次握手应改变结论");
    assert_eq!(bridge.snapshot().compat, UiCompat::Compatible);
    assert!(!bridge.check_compat().await, "同一 epoch 只握手一次");

    // 桥走的是认证连接而不是 user：whoami 说是前端，UserOnly 方法被 -32016 拒。
    let whoami = bridge.raw("whoami", serde_json::json!({})).await.unwrap();
    assert_eq!(whoami["principal"], frontend_principal());
    let denied = bridge
        .raw(
            "revealPluginToken",
            serde_json::json!({"id": FRONTEND_ID, "kind": "frontend"}),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.rpc_code, Some(-32016));

    // 文件夹：角色、展示名、计数。
    let mut folders = bridge.list_folders("demo").await.unwrap();
    folders.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(folders.len(), 2);
    assert_eq!(folders[0].name, "Archive");
    assert_eq!(folders[0].role, "archive");
    assert_eq!(folders[0].total, 1);
    assert_eq!(folders[1].display_name, "收件箱");
    assert_eq!(folders[1].total, 3);
    assert_eq!(folders[1].unseen, 1);

    // 列表：按 sort_at 倒序，身份带 folder。
    let page = bridge.list_messages("demo", "INBOX", 0, 50).await.unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(
        page.messages
            .iter()
            .map(|row| row.subject.as_str())
            .collect::<Vec<_>>(),
        vec!["最新的一封", "带 HTML 与附件", "最早的一封"]
    );
    assert!(page.messages.iter().all(|row| row.folder == "INBOX"));
    assert!(!page.messages[2].seen);
    assert!(page.messages[1].has_attachments);
    let second = bridge.list_messages("demo", "INBOX", 2, 50).await.unwrap();
    assert_eq!(second.messages.len(), 1);
    assert_eq!(second.total, 3);

    // 详情：文本正文、HTML 只报存在、附件只有元数据（含 stored 位）、内核的远程内容结论。
    let message = bridge.get_message("demo", "INBOX", "2").await.unwrap();
    assert_eq!(message.body, "带 HTML 与附件 的正文");
    assert!(message.has_html);
    assert_eq!(message.attachments.len(), 3);
    assert_eq!(message.attachments[0].filename, "印刷确认.pdf");
    assert!(message.attachments[0].stored);
    assert!(!message.attachments[2].stored, "超限 part 如实说没存");
    assert_eq!(message.remote_content.decision, "block");
    assert_eq!(message.remote_content.reason, "default");
    let json = serde_json::to_value(&message).unwrap();
    assert!(json.get("body_html").is_none(), "{json}");
    assert!(json.get("content").is_none());

    // ADR-0033 决策 3/4：准备隔离文档——脚本没了、cid 变成带令牌的 part URL、远程图片被拦、
    // 链接变惰性；part 通道只认令牌与本封的已落库 part；释放后 404。
    let documents = DocumentStore::new();
    let base = ProtocolBase::for_platform();
    let document = bridge
        .prepare_document(&documents, &base, "demo", "INBOX", "2", false)
        .await
        .unwrap();
    let token = document.token.clone().expect("有 HTML 就有令牌");
    assert_eq!(document.url.as_deref(), Some(base.doc_url(&token).as_str()));
    assert!(document.has_html && !document.truncated && !document.allow_remote);
    assert_eq!(
        (
            document.remote_refs,
            document.cid_resolved,
            document.cid_missing,
            document.links
        ),
        (1, 1, 0, 1)
    );
    let stored = documents.get(&token).expect("文档在表里");
    assert!(!stored.html.contains("<script"), "{}", stored.html);
    // 远程地址只以惰性证据 `data-maild-src` 存在，没有任何会发请求的属性。
    assert!(!stored.html.contains(" src=\"https://"), "{}", stored.html);
    assert!(stored
        .html
        .contains("data-maild-src=\"https://remote.example/t.gif\""));
    assert!(stored
        .html
        .contains(&format!("{}/part/{token}/3", base.url_prefix)));
    assert!(stored
        .html
        .contains(&format!("{}/link/{token}/0", base.url_prefix)));
    assert_eq!(stored.links[0].href, "https://shop.example/x");
    assert_eq!(stored.identity.uid, "2");
    assert_eq!(stored.parts.len(), 2, "只有已落库的 part 可经通道取回");
    // 协议路由 → part 字节。
    match route(&format!("/part/{token}/3")) {
        Route::Part(routed, part) => {
            let (mime, bytes) = bridge.serve_part(&documents, &routed, part).await.unwrap();
            assert_eq!(mime, "image/png");
            assert_eq!(bytes, b"png!");
        }
        other => panic!("{other:?}"),
    }
    let (mime, bytes) = bridge.serve_part(&documents, &token, 2).await.unwrap();
    assert_eq!(mime, "application/pdf");
    assert_eq!(bytes.len(), 2048);
    assert_eq!(
        bridge
            .serve_part(&documents, &token, 4)
            .await
            .unwrap_err()
            .code,
        UiErrorCode::NotFound,
        "未落库的 part 不给"
    );
    assert_eq!(
        bridge
            .serve_part(&documents, &token, 9)
            .await
            .unwrap_err()
            .code,
        UiErrorCode::NotFound,
        "不是本封的 part"
    );
    let other_token = maild_ui::documents::new_token();
    assert_eq!(
        bridge
            .serve_part(&documents, &other_token, 3)
            .await
            .unwrap_err()
            .code,
        UiErrorCode::NotFound,
        "别的令牌拿不到"
    );
    // 放行远程：同一封信另一份文档，远程 src 保留，CSP 放图片。
    let allowed = bridge
        .prepare_document(&documents, &base, "demo", "INBOX", "2", true)
        .await
        .unwrap();
    let allowed_doc = documents.get(allowed.token.as_deref().unwrap()).unwrap();
    assert!(allowed_doc
        .html
        .contains("src=\"https://remote.example/t.gif\""));
    assert!(allowed_doc.csp.contains("https: http:"));
    assert!(!stored.csp.contains("https:"));
    // 释放：文档与 part 都拿不到。
    assert!(documents.release(&token));
    assert!(documents.get(&token).is_none());
    assert_eq!(
        bridge
            .serve_part(&documents, &token, 3)
            .await
            .unwrap_err()
            .code,
        UiErrorCode::NotFound
    );
    // 没有 HTML 的信：没有令牌，UI 显示纯文本。
    let plain = bridge
        .prepare_document(&documents, &base, "demo", "INBOX", "1", false)
        .await
        .unwrap();
    assert!(plain.token.is_none() && !plain.has_html);
    // 附件字节经桥直接读（保存用）：走认证连接，护栏同 client。
    let saved = bridge
        .read_attachment("demo", "INBOX", "2", 2)
        .await
        .unwrap();
    assert_eq!(saved.bytes.len(), 2048);
    assert_eq!(saved.attachment.filename, "印刷确认.pdf");
    assert_eq!(
        bridge
            .read_attachment("demo", "INBOX", "2", 4)
            .await
            .unwrap_err()
            .code,
        UiErrorCode::NotFound
    );
    // 阅读策略经桥可读可改（前端声明了 settings 路由）。
    let policy = bridge.reading_policy().await.unwrap();
    assert_eq!(policy.remote_content, "block");
    assert!(policy.persisted);
    let policy = bridge
        .set_reading_policy(&maild_ui::bridge::UiReadingPolicyPatch {
            trust: vec!["Editor@example.test".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(policy.trusted_senders[0].value, "editor@example.test");
    let message = bridge.get_message("demo", "INBOX", "2").await.unwrap();
    assert_eq!(message.remote_content.reason, "trusted_address");

    // 错误投影。
    let missing = bridge
        .get_message("demo", "INBOX", "999")
        .await
        .unwrap_err();
    assert_eq!(missing.code, UiErrorCode::NotFound);
    let invalid = bridge
        .list_messages("demo", "INBOX", 0, 0)
        .await
        .unwrap_err();
    assert_eq!(invalid.code, UiErrorCode::Invalid);
    let invalid = bridge.list_folders("").await.unwrap_err();
    assert_eq!(invalid.code, UiErrorCode::Invalid);

    // 自身在 daemon 眼里的样子——设置页据此显示授权。
    let info = bridge.frontend_info().await.unwrap();
    assert_eq!(info.id, FRONTEND_ID);
    // 测试配置仍写旧的 `default = "builtin-egui"`：读取时迁移为内置 Tauri 前端（ADR-0039），
    // 端到端钉住升级用户不必改 TOML 就会打开新界面。
    assert!(info.is_default);
    assert_eq!(info.default_frontend, FRONTEND_ID);
    for word in [
        "mailbox_write",
        "accounts_manage",
        "addresses_manage",
        "plugins_manage",
        "notifications_read",
    ] {
        assert!(info.permissions.iter().any(|p| p == word), "{word}");
    }

    // ADR-0014 seam：user 排一条 intent，前端 claim 到它；没有就是 None。
    assert_eq!(bridge.claim_intent().await.unwrap(), None);
    let epoch_before = bridge.subscribe().borrow().intent_epoch;
    let user = RpcClient::new(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    // 另一条 user 长连接同时在听推送：它绝不能收到 `frontend/intent`（ADR-0045）。
    let mut eavesdropper = user.connect_stream().await.unwrap();
    user.call(
        "openFrontend",
        serde_json::json!({"frontend": FRONTEND_ID, "intent": {"route": "inbox", "account": "demo"}}),
    )
    .await
    .unwrap();
    // ADR-0045：入队即经认证长连接实时推 `frontend/intent`，连接 actor 递增 intent_epoch——
    // 隐藏在菜单栏的窗口靠它回到前台，不依赖 Reopen。
    let mut epochs = bridge.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        while epochs.borrow_and_update().intent_epoch <= epoch_before {
            epochs.changed().await.unwrap();
        }
    })
    .await
    .expect("frontend/intent 推送应在 5s 内递增 intent_epoch");
    assert_eq!(bridge.subscribe().borrow().intent_epoch, epoch_before + 1, "一次入队一次唤醒");
    let mut overheard = Vec::new();
    eavesdropper
        .call("status", serde_json::json!({}), |notification| {
            overheard.push(notification.method)
        })
        .await
        .unwrap();
    assert!(
        !overheard.iter().any(|method| method == "frontend/intent"),
        "user 连接不该收到意图唤醒: {overheard:?}"
    );
    drop(eavesdropper);
    assert_eq!(
        bridge.claim_intent().await.unwrap(),
        Some(OpenIntent::Inbox {
            account: Some("demo".into())
        })
    );

    // daemon 退出：桥转离线，读操作得到 offline 而不是含路径的原文。
    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
    wait_for(&bridge, |snapshot| snapshot.phase != "online").await;
    let offline = bridge.list_folders("demo").await.unwrap_err();
    assert_eq!(offline.code, UiErrorCode::Offline);
    assert!(!offline.message.contains(socket.to_str().unwrap()));
    assert_eq!(bridge.snapshot().principal, None, "主体随连接消亡");

    bridge.shutdown().await;
}

/// ADR-0046 跨 crate 端到端：真 maild（平台 Agent 形态，带受管停机服务）+ 以
/// `frontend:<FRONTEND_ID>` 认证的 maild-app 连接。user 侧 `stopManagedAgent`（菜单栏「退出 <产品名>」）
/// 必须在排空窗口内经认证长连接推到前端、把 `ViewState.shutdown_requested` 置位——此时 daemon
/// 还活着、连接仍在线；随后 daemon 正常退出并清掉 socket。旁听的 user 长连接收不到停机推送。
#[tokio::test(flavor = "current_thread")]
async fn stop_managed_agent_reaches_the_frontend_before_the_daemon_exits() {
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    install_frontend_package(&paths);
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    let options = DaemonOptions::new(paths.config_file.clone(), paths.clone())
        .expect("绝对路径")
        .with_managed_agent_control();
    let mut task: DaemonTask = tokio::spawn(run_daemon_until(options, std::future::pending()));
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;

    let bridge = Bridge::spawn(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    wait_for(&bridge, |snapshot| snapshot.phase == "online").await;
    assert_eq!(bridge.snapshot().principal, Some(frontend_principal()));
    let mut state = bridge.subscribe();
    assert!(!state.borrow_and_update().shutdown_requested);

    let user = RpcClient::new(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    let mut eavesdropper = user.connect_stream().await.unwrap();
    let mut overheard = Vec::new();
    // 先完成一次往返，确保 user 流会话已订阅推送，之后的停机帧若泄漏一定会被看到。
    eavesdropper
        .call("status", serde_json::json!({}), |notification| {
            overheard.push(notification.method)
        })
        .await
        .unwrap();

    let stopped = user
        .call("stopManagedAgent", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(stopped["stopping"], true);

    // 排空窗口内：前端先看到停机信号，而 daemon 仍在运行、连接仍在线。
    tokio::time::timeout(Duration::from_secs(1), async {
        while !state.borrow_and_update().shutdown_requested {
            state.changed().await.expect("停机信号到达前 actor 不应消亡");
        }
    })
    .await
    .expect("agent/stopping 应在排空窗口内置位 shutdown_requested");
    assert!(
        !task.is_finished(),
        "shutdown_requested 必须先于 daemon 退出到达（排空窗口）"
    );
    let at_signal = bridge.snapshot();
    assert_eq!(at_signal.phase, "online", "信号在流拆除前到达: {at_signal:?}");

    // user 长连接在排空窗口内仍可往返，但绝不会看到 agent/stopping。
    let _ = eavesdropper
        .call("status", serde_json::json!({}), |notification| {
            overheard.push(notification.method)
        })
        .await;
    assert!(
        !overheard.iter().any(|method| method == "agent/stopping"),
        "user 连接不该收到停机推送: {overheard:?}"
    );
    drop(eavesdropper);

    // daemon 随后正常退出（无需外部 shutdown 信号），socket 被清理。
    tokio::time::timeout(Duration::from_secs(5), &mut task)
        .await
        .expect("daemon 应在排空窗口后自行退出")
        .unwrap()
        .unwrap();
    assert!(!socket.exists(), "受管停机后 socket 应被清理");
    // 停机信号一旦置位不回落：断线后快照仍记得要退出。
    wait_for(&bridge, |snapshot| snapshot.phase != "online").await;
    assert!(bridge.subscribe().borrow().shutdown_requested);

    bridge.shutdown().await;
}

/// 包未安装：不回落 user，不静默。原因单独可见，好让 UI 给出「去安装」。
#[tokio::test(flavor = "current_thread")]
async fn an_uninstalled_shell_reports_the_missing_package_instead_of_running_as_user() {
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;

    let bridge = Bridge::spawn(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    wait_for(&bridge, |snapshot| snapshot.identity_failure.is_some()).await;
    let snapshot = bridge.snapshot();
    assert!(
        snapshot
            .identity_failure
            .as_deref()
            .unwrap()
            .contains("未安装"),
        "{snapshot:?}"
    );
    assert_ne!(snapshot.phase, "online");
    assert_eq!(snapshot.principal, None);
    let error = bridge.list_folders("demo").await.unwrap_err();
    assert_eq!(error.code, UiErrorCode::Offline);

    bridge.shutdown().await;
    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

/// HTTP 端点承载不了前端主体：spawn 即拒，不静默降级。
#[tokio::test(flavor = "current_thread")]
async fn an_http_endpoint_is_refused_at_spawn() {
    let error = Bridge::spawn(Endpoint::Http {
        url: "http://127.0.0.1:9/rpc".into(),
        token: "t".repeat(32),
    })
    .err()
    .expect("HTTP 必须被拒");
    assert!(error.message.contains("IPC"), "{}", error.message);
}

#[allow(dead_code)]
#[path = "../../maild/tests/common/fake_mail.rs"]
mod fake_mail;

#[tokio::test(flavor = "current_thread")]
async fn authenticated_compose_upload_and_send_reach_real_smtp_and_survive_restart() {
    use maild::account_catalog::*;
    std::env::set_var("MAILD_TEST_CREDENTIAL_STORE", "memory");
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");
    let server = fake_mail::FakeMail::start_imap_implicit_and_smtp_starttls()
        .await
        .unwrap();
    server.add_mailbox("Sent", "\\Sent");
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    install_frontend_package(&paths);
    let id = generate_account_id();
    let profile = AccountProfile {
        id: id.clone(),
        revision: 1,
        display_name: "测试发件人".into(),
        address: "writer@example.test".into(),
        enabled: true,
        archived_at: None,
        incoming_credential_revision: Some(1),
        outgoing_credential_revision: None,
        incoming: IncomingSettings::Imap {
            server: "127.0.0.1".into(),
            port: server.imap_addr.port(),
            security: SecurityMode::Implicit,
            username: "writer@example.test".into(),
            mailbox: "INBOX".into(),
            idle: true,
            poll_secs: 300,
        },
        outgoing: Some(OutgoingSettings::Smtp {
            server: "127.0.0.1".into(),
            port: server.smtp_addr.port(),
            security: SecurityMode::StartTls,
            username: "writer@example.test".into(),
            credential: CredentialSharing::Incoming,
        }),
    };
    save_catalog(
        &paths.accounts_file,
        &AccountCatalog {
            format_version: CATALOG_FORMAT_VERSION,
            profiles: vec![profile],
        },
    )
    .unwrap();
    maild::credential::platform_store_for(&paths.accounts_file)
        .save(
            &CredentialKey::new(&id, CredentialRole::Incoming, 1),
            b"test-only-secret",
        )
        .unwrap();
    let db = paths.data_dir.join("mail.sqlite3");
    {
        let s = Store::open(db.to_str().unwrap()).unwrap();
        s.upsert_folder(&id, "Sent", "Sent", Some(FolderRole::Sent))
            .unwrap();
    }
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let bridge = Bridge::spawn(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    wait_for(&bridge, |s| s.phase == "online").await;
    let senders = bridge.senders().await.unwrap();
    assert_eq!(senders[0].address, "writer@example.test");
    assert!(!serde_json::to_string(&senders)
        .unwrap()
        .contains("test-only-secret"));
    let draft = bridge
        .new_draft(
            &id,
            maild_client::DraftPatchInput {
                from: Some("writer@example.test".into()),
                to: Some(vec!["reader@example.test".into()]),
                bcc: Some(vec!["hidden@example.test".into()]),
                body: Some("中文正文".into()),
                subject: Some("真正的桥接投递".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let bytes = vec![42u8; 1024 * 1024 + 31];
    let path = directory.path().join("附件.bin");
    std::fs::write(&path, &bytes).unwrap();
    let files = maild_ui::compose_files::FileSelections::default();
    let picked = files.select(&path).unwrap();
    let projected = serde_json::to_string(&picked).unwrap();
    assert!(!projected.contains(directory.path().to_str().unwrap()));
    let upload = files
        .upload(&bridge, &draft.id, &picked.token, |_| {})
        .await
        .unwrap();
    let draft = bridge.finish_upload(&upload, draft.version).await.unwrap();
    files.release(&picked.token);
    let error = bridge
        .save_draft(
            &draft.id,
            1,
            maild_client::DraftPatchInput {
                body: Some("stale".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, UiErrorCode::Conflict);
    let sent = bridge
        .deliver_draft(&draft.id, draft.version)
        .await
        .unwrap();
    assert_eq!(sent.send_state, "sent");
    assert_eq!(sent.sent_archive, Some(true));
    assert_eq!(
        bridge
            .deliver_draft(&draft.id, sent.version)
            .await
            .unwrap_err()
            .code,
        UiErrorCode::Locked
    );
    let raw = server.messages();
    assert_eq!(raw.len(), 1);
    assert!(!String::from_utf8_lossy(&raw[0]).contains("hidden@example.test"));
    // The shared MIME round-trip tests cover encoding; here native upload and real SMTP must preserve bytes.
    let content = Store::open(db.to_str().unwrap());
    assert!(content.is_err(), "live daemon still owns its DB");
    bridge.shutdown().await;
    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
    let reopened = Store::open(db.to_str().unwrap()).unwrap();
    assert_eq!(
        reopened.get_draft(&draft.id).unwrap().unwrap().send_state,
        "sent"
    );
    assert_eq!(
        reopened
            .draft_file_content(&draft.id, &upload)
            .unwrap()
            .unwrap()
            .content,
        bytes
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mailbox_workspace_queries_writes_threads_and_large_activity_use_frontend_identity() {
    use maild::pending::OpState;
    use maild_ui::mailbox::{MailboxQuery, MailboxWrite};
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    install_frontend_package(&paths);
    {
        let store = Store::open(paths.data_dir.join("mail.sqlite3").to_str().unwrap()).unwrap();
        for (account, folder, role) in [
            ("a", "Incoming", FolderRole::Inbox),
            ("b", "INBOX", FolderRole::Inbox),
            ("a", "Sent", FolderRole::Sent),
            ("a", "Archive", FolderRole::Archive),
        ] {
            store
                .upsert_folder(account, folder, folder, Some(role))
                .unwrap();
        }
        let mut root = seeded("a", "Incoming", "7", "印刷确认", 1000);
        root.message_id = "press-root@test".into();
        store.insert_message(&root).unwrap();
        let mut reply = seeded("a", "Sent", "7", "Re: 印刷确认", 1100);
        reply.message_id = "press-reply@test".into();
        reply.in_reply_to = "press-root@test".into();
        store.insert_message(&reply).unwrap();
        store
            .insert_message(&seeded("b", "INBOX", "7", "另一个账户", 1050))
            .unwrap();
        store
            .queue_append_op(
                "a",
                "Sent",
                &format!("Subject: not-for-js\r\n\r\n{}", "z".repeat(2 * 1024 * 1024)),
                1200,
            )
            .unwrap();
        let op = store.claim_next_op("a", 1201).unwrap().unwrap();
        store
            .finish_op(
                &op,
                OpState::Unknown,
                "AUTH PLAIN hidden-password MIME secret-body",
                1202,
            )
            .unwrap();
    }
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let bridge = Bridge::spawn(Endpoint::Unix { path: socket }).unwrap();
    wait_for(&bridge, |s| s.phase == "online").await;
    assert_eq!(bridge.snapshot().principal, Some(frontend_principal()));
    let page = bridge
        .mailbox_query(
            MailboxQuery {
                role: Some("inbox".into()),
                ..Default::default()
            },
            0,
        )
        .await
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.messages.len(), 2);
    let search = bridge
        .mailbox_query(
            MailboxQuery {
                search: true,
                query: Some("印刷".into()),
                ..Default::default()
            },
            0,
        )
        .await
        .unwrap();
    assert_eq!(search.total, 2);
    let located = bridge
        .mailbox_locate("a".into(), "Sent".into(), "7".into())
        .await
        .unwrap();
    assert_eq!(located.summary.subject, "Re: 印刷确认");
    let thread = bridge
        .mailbox_thread("a".into(), "Incoming".into(), "7".into(), 0)
        .await
        .unwrap();
    assert_eq!(thread.total, 2);
    assert_eq!(thread.messages[1].folder, "Sent");
    let receipt = bridge
        .mailbox_write(MailboxWrite {
            action: "mark".into(),
            account: "a".into(),
            folder: "Incoming".into(),
            uids: vec!["7".into()],
            flag: Some("flagged".into()),
            value: Some(true),
            target: None,
        })
        .await
        .unwrap();
    assert_eq!(receipt.queued, 1);
    let starred = bridge
        .mailbox_query(
            MailboxQuery {
                flagged: Some(true),
                ..Default::default()
            },
            0,
        )
        .await
        .unwrap();
    assert_eq!(starred.total, 1);
    assert_eq!(starred.messages[0].account, "a");
    let activity = bridge.mailbox_activity(None, None).await.unwrap();
    assert!(activity
        .ops
        .iter()
        .any(|o| o.subject.as_deref() == Some("印刷确认")));
    let append = activity.ops.iter().find(|o| o.kind == "append").unwrap();
    assert_eq!(append.state, "unknown");
    let wire = serde_json::to_string(&activity).unwrap();
    assert!(!wire.contains("payload"));
    assert!(!wire.contains("not-for-js"));
    assert!(!wire.contains("hidden-password"));
    assert!(!wire.contains("secret-body"));
    assert!(wire.len() < 10000);
    let resolved = bridge
        .mailbox_resolve(append.id, "discard".into())
        .await
        .unwrap();
    assert_eq!(resolved.state, "failed");
    assert!(!serde_json::to_string(&resolved)
        .unwrap()
        .contains("payload"));
    let after = bridge
        .mailbox_activity(None, Some(activity.ops[0].id))
        .await
        .unwrap();
    assert!(after.ops.iter().all(|o| o.id < activity.ops[0].id));
    assert!(bridge
        .raw(
            "listMessageSummaries",
            serde_json::json!({"role":"made-up"})
        )
        .await
        .is_err());
    bridge.shutdown().await;
    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn activity_header_projection_requires_read_authority_as_well_as_mailbox_write() {
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    install_frontend_package(&paths);
    let manifest = paths.frontend_dir.join(FRONTEND_ID).join("frontend.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace(
            "capabilities = [\"inbox\", \"message\", \"compose\", \"settings\", \"background\"]",
            "capabilities = [\"background\"]",
        ),
    )
    .unwrap();
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let client = RpcClient::new(Endpoint::Unix { path: socket }).unwrap();
    let token = client
        .reveal_plugin_token(FRONTEND_ID, Some("frontend"))
        .await
        .unwrap();
    let mut connection = client.connect_stream().await.unwrap();
    connection.authenticate(&token).await.unwrap();
    assert!(connection
        .call("listPendingOps", serde_json::json!({}), |_| {})
        .await
        .is_ok());
    for (method, params) in [
        ("listPendingOps", serde_json::json!({"summary_only":true})),
        (
            "resolvePendingOp",
            serde_json::json!({"id":1,"resolution":"retry","summary_only":true}),
        ),
    ] {
        let error = connection.call(method, params, |_| {}).await.unwrap_err();
        assert_eq!(error.rpc_code, Some(-32016));
    }
    drop(connection);
    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn version_mismatch_blocks_writes_keeps_reads_and_recovers_on_reconnect() {
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths);
    seed_store(&paths);
    install_frontend_package(&paths);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;

    let bridge = Bridge::spawn(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    wait_for(&bridge, |snapshot| snapshot.phase == "online").await;

    // 模拟「新前端 + 旧后台」：要求更高协议代，握手判 KernelOutdated。
    assert!(
        bridge
            .check_compat_against(maild_client::PROTOCOL_VERSION + 1)
            .await
    );
    assert_eq!(bridge.snapshot().compat, UiCompat::KernelOutdated);

    // 只读查询继续工作。
    assert_eq!(bridge.list_folders("demo").await.unwrap().len(), 2);
    assert_eq!(
        bridge.list_messages("demo", "INBOX", 0, 50).await.unwrap().total,
        3
    );
    assert!(bridge.reading_policy().await.is_ok());
    assert!(bridge.drafts(0).await.is_ok());
    assert!(bridge.settings_packages().await.is_ok());

    // 写信、标记、策略修改、包管理与暂存全部在桥内挡下，不改动后台。
    assert_eq!(
        bridge
            .mailbox_write(maild_ui::mailbox::MailboxWrite {
                action: "mark".into(),
                account: "demo".into(),
                folder: "INBOX".into(),
                uids: vec!["1".into()],
                flag: Some("seen".into()),
                value: Some(true),
                target: None,
            })
            .await
            .err()
            .unwrap()
            .code,
        UiErrorCode::Incompatible
    );
    assert_eq!(
        bridge
            .new_draft("demo", maild_client::DraftPatchInput::default())
            .await
            .unwrap_err()
            .code,
        UiErrorCode::Incompatible
    );
    assert_eq!(
        bridge
            .set_reading_policy(&maild_ui::bridge::UiReadingPolicyPatch {
                remote_content: Some("allow".into()),
                trust: vec![],
                untrust: vec![],
            })
            .await
            .unwrap_err()
            .code,
        UiErrorCode::Incompatible
    );
    assert_eq!(
        bridge
            .settings_package_action(maild_ui::settings::PackageAction::Refresh)
            .await
            .unwrap_err()
            .code,
        UiErrorCode::Incompatible
    );
    let selections = maild_ui::settings_packages::PackageSelections::default();
    let candidate = directory.path().join("candidate-pkg");
    std::fs::create_dir_all(&candidate).unwrap();
    assert_eq!(
        selections
            .select(&bridge, &candidate, "frontend")
            .await
            .err()
            .unwrap()
            .code,
        UiErrorCode::Incompatible
    );
    assert!(
        !paths.frontend_dir.join(".incoming").exists(),
        "版本不一致时不得在前端目录创建 staging"
    );

    // 原生修复入口不被版本闸门挡下（测试进程不在主 app bundle 内，走激活失败提示而非 Incompatible）。
    let recovery_err = bridge
        .settings_package_action(maild_ui::settings::PackageAction::Recovery)
        .await
        .unwrap_err();
    assert_ne!(recovery_err.code, UiErrorCode::Incompatible);

    // 原先未读的 UID 1 仍为未读，证明写请求根本没有发给后台。
    let detail = bridge.get_message("demo", "INBOX", "1").await.unwrap();
    assert!(!detail.summary.seen);

    // 重启后台产生新连接 epoch：同代重握手恢复 Compatible，写入随之放行。
    let first_epoch = bridge.snapshot().connection_epoch;
    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
    let (mut task2, shutdown2) = spawn_daemon(&paths);
    wait_socket_or_fail(&mut task2, &socket).await;
    wait_for(&bridge, |s| {
        s.phase == "online" && s.connection_epoch > first_epoch
    })
    .await;
    assert!(bridge.check_compat().await);
    assert_eq!(bridge.snapshot().compat, UiCompat::Compatible);
    let updated = bridge
        .set_reading_policy(&maild_ui::bridge::UiReadingPolicyPatch {
            remote_content: Some("allow".into()),
            trust: vec![],
            untrust: vec![],
        })
        .await
        .unwrap();
    assert_eq!(updated.remote_content, "allow");

    bridge.shutdown().await;
    let _ = shutdown2.send(());
    task2.await.unwrap().unwrap();
}
