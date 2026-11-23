//! 全栈汇合验收（ADR-0041/0042/0043 × ADR-0031/0033）：真 daemon + 本地假 IMAP，无窗口。
//!
//! 钉住的闭环：Tauri 前端主体 `frontend:<FRONTEND_ID>` 经桥
//! 1. `ui_sync_mailbox` 敲醒后台立即同步；
//! 2. `ui_discover_provider` / `ui_settings_discover_provider` 识别服务商（零网络预设）；
//! 3. `ui_server_search` 在服务器上找到冷启动窗口之外的历史邮件，
//!    `ui_fetch_server_message` 把它拉回本地库（投影里没有 `body_html`），
//!    随后 `ui_prepare_document` 照常为它生成隔离 HTML 文档；
//! 4. `status.runtime` 低占用指标与 `getSettings` 的新邮件通知字段经同一主体可读。

use std::time::Duration;

use maild::{run_daemon_until, DaemonOptions};
use maild_client::Endpoint;
use maild_paths::AppPaths;
use maild_ui::bridge::UiErrorCode;
use maild_ui::documents::{DocumentStore, ProtocolBase};
use maild_ui::mailbox::UiMailboxSyncOutcome;
use maild_ui::{Bridge, FRONTEND_ID};
use serde_json::json;

#[path = "../../maild/tests/common/fake_mail.rs"]
#[allow(dead_code)]
mod fake_mail;

type DaemonTask = tokio::task::JoinHandle<Result<(), maild::DaemonError>>;

fn test_paths(root: &std::path::Path) -> AppPaths {
    AppPaths::from_roots(
        &root.join("config"),
        &root.join("data"),
        Some(&root.join("runtime")),
        Some(&root.join("state")),
        root,
    )
}

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
[plugins.rules.config]
notify_new_mail = true

[[accounts]]
account = "legacy"
server = "127.0.0.1"
port = {imap_port}
username = "dev"
password = "dev"
initial_sync_limit = 2
"#
        ),
    )
    .unwrap();
}

fn install_frontend_package(paths: &AppPaths) {
    let package = paths.frontend_dir.join(FRONTEND_ID);
    std::fs::create_dir_all(&package).unwrap();
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("package/frontend.toml"),
    )
    .unwrap();
    std::fs::write(
        package.join("frontend.toml"),
        manifest.replace(maild_brand::BUNDLED_FRONTEND_ID, FRONTEND_ID),
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

fn plain_mail(subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: Sender <sender@test.local>\r\nTo: me@test.local\r\n\
         Subject: {subject}\r\nDate: Tue, 1 Mar 2022 10:00:00 +0000\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

fn html_mail(subject: &str, marker: &str) -> Vec<u8> {
    format!(
        "From: Sender <sender@test.local>\r\nTo: me@test.local\r\n\
         Subject: {subject}\r\nDate: Mon, 1 Mar 2021 10:00:00 +0000\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/alternative; boundary=\"b1\"\r\n\r\n\
         --b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{marker} 纯文本\r\n\
         --b1\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
         <html><head><script>alert(1)</script></head><body><p>{marker} 版式</p>\
         <img src=\"https://tracker.example/p.gif\"><a href=\"https://shop.example/x\">链接</a>\
         </body></html>\r\n--b1--\r\n"
    )
    .into_bytes()
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

async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("超时: {what}");
}

#[tokio::test(flavor = "current_thread")]
async fn tauri_frontend_syncs_discovers_and_pulls_server_history_into_an_isolated_document() {
    let server = fake_mail::FakeMail::start().await.unwrap();
    // UID 1 是带 HTML 的老信，UID 2..=4 是普通信；冷启动窗口 = 最新 2 封（3、4）。
    server.deliver_to("INBOX", "\\Seen", html_mail("三年前的发票", "needle-history"));
    for index in 2..=4 {
        server.deliver_to(
            "INBOX",
            "\\Seen",
            plain_mail(&format!("近况 {index}"), &format!("body-{index}")),
        );
    }

    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths, server.imap_addr.port());
    install_frontend_package(&paths);
    let (mut task, shutdown) = spawn_daemon(&paths);
    let socket = paths.runtime_dir.join("mail.sock");
    wait_socket_or_fail(&mut task, &socket).await;
    let bridge = Bridge::spawn(Endpoint::Unix { path: socket }).unwrap();

    // 冷启动窗口落库：只有 3、4；1 不在本地。
    eventually("冷启动窗口落库", || async {
        bridge
            .list_messages("legacy", "INBOX", 0, 50)
            .await
            .is_ok_and(|page| page.total == 2)
    })
    .await;
    let local_uids: Vec<String> = bridge
        .list_messages("legacy", "INBOX", 0, 50)
        .await
        .unwrap()
        .messages
        .into_iter()
        .map(|summary| summary.uid)
        .collect();
    assert_eq!(local_uids, ["4", "3"]);
    assert_eq!(
        bridge
            .get_message("legacy", "INBOX", "1")
            .await
            .unwrap_err()
            .code,
        UiErrorCode::NotFound
    );

    // 1. 立即同步：前端主体可敲门（messages_read），未知账户是参数错误，不是授权错误。
    bridge.sync_mailbox(None).await.unwrap();
    bridge.sync_mailbox(Some("legacy".into())).await.unwrap();
    assert_eq!(
        bridge
            .sync_mailbox(Some("nobody".into()))
            .await
            .unwrap_err()
            .code,
        UiErrorCode::Invalid
    );
    let raw = bridge.raw("syncMessages", json!({})).await.unwrap();
    assert_eq!(raw["triggered"], json!(["legacy"]));
    assert_eq!(raw["online_count"], 1);
    // 敲门后新到的信照常落库（IDLE 或被打断的等待都会把它带回来）。
    server.deliver_to("INBOX", "", plain_mail("刚到的信", "body-5"));
    bridge.sync_mailbox(Some("legacy".into())).await.unwrap();
    eventually("立即同步带回新信", || async {
        bridge.get_message("legacy", "INBOX", "5").await.is_ok()
    })
    .await;
    server.add_mailbox("Archive", "\\Archive");
    server.deliver_to("Archive", "", plain_mail("新文件夹邮件", "body-archive"));
    let outcome = bridge
        .sync_mailbox_scope("legacy".into(), Some("Archive".into()))
        .await
        .unwrap();
    assert_eq!(outcome, UiMailboxSyncOutcome::Complete);
    assert!(bridge.get_message("legacy", "Archive", "1").await.is_ok());
    let folders = bridge.list_folders("legacy").await.unwrap();
    assert_eq!(
        folders.iter().find(|folder| folder.name == "INBOX").unwrap().is_primary,
        Some(true)
    );
    assert_eq!(
        folders.iter().find(|folder| folder.name == "Archive").unwrap().is_primary,
        Some(false)
    );

    // ADR-0048 决策 7：回复引用块跟随桥传来的界面语言；不传时按 daemon 偏好（跟随系统语言）。
    let english = bridge
        .reply_draft("legacy", "INBOX", "4", "reply", Some("en"))
        .await
        .unwrap();
    assert!(english.body.contains(" wrote:"), "{}", english.body);
    let chinese = bridge
        .reply_draft("legacy", "INBOX", "4", "reply", Some("zh-CN"))
        .await
        .unwrap();
    assert!(chinese.body.contains("写道："), "{}", chinese.body);
    // 不传 locale 时用 daemon 的 effective，跟随本机系统语言，所以只比较两者一致，不假定中文。
    let preferred = bridge
        .reply_draft("legacy", "INBOX", "4", "reply", None)
        .await
        .unwrap();
    let invalid = bridge
        .reply_draft("legacy", "INBOX", "4", "reply", Some("not a tag"))
        .await
        .unwrap();
    assert_eq!(invalid.body, preferred.body, "非法标签按旧后台处理：去掉 locale 重试");

    // 2. 服务商识别：预设命中零网络；无法识别的输入投影为 None 而不是错误。
    let qq = bridge
        .discover_provider("alice@qq.com".into(), false)
        .await
        .unwrap();
    assert_eq!(qq.provider_id, "qq");
    assert_eq!(qq.incoming.server, "imap.qq.com");
    assert_eq!(qq.suggested_username, "alice@qq.com");
    let gmail = bridge
        .settings_discover_provider("bob@gmail.com".into())
        .await
        .unwrap()
        .expect("预设命中");
    assert_eq!(gmail.incoming.server, "imap.gmail.com");
    assert_eq!(gmail.incoming.port, 993);
    assert_eq!(gmail.source, "preset");
    assert!(gmail.outgoing.is_some());
    assert_eq!(
        bridge
            .settings_discover_provider("not an address".into())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        bridge
            .discover_provider("   ".into(), false)
            .await
            .unwrap_err()
            .code,
        UiErrorCode::Invalid
    );

    // 3. 服务器检索 → 按 UID 拉回 → 安全详情 → 隔离文档。
    let hits = bridge
        .server_search("legacy".into(), "INBOX".into(), "needle-history".into())
        .await
        .unwrap();
    assert_eq!(
        hits.iter().map(|hit| hit.uid.as_str()).collect::<Vec<_>>(),
        ["1"]
    );
    assert_eq!(hits[0].subject, "三年前的发票");
    assert!(
        bridge
            .mailbox_server_search(
                "legacy".into(),
                "INBOX".into(),
                "needle-history".into(),
                Some(hits[0].sort_at + 1),
                None,
            )
            .await
            .unwrap()
            .is_empty(),
        "since 晚于命中邮件 sort_at 时应被服务端过滤"
    );
    assert_eq!(
        bridge
            .mailbox_server_search(
                "legacy".into(),
                "INBOX".into(),
                "needle-history".into(),
                Some(hits[0].sort_at),
                Some(hits[0].sort_at),
            )
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        bridge
            .get_message("legacy", "INBOX", "1")
            .await
            .is_err(),
        "服务器检索只给摘要，不落库"
    );

    let fetched = bridge
        .fetch_server_message("legacy".into(), "INBOX".into(), "1".into())
        .await
        .unwrap()
        .expect("服务器上有 UID 1");
    assert_eq!(fetched.summary.subject, "三年前的发票");
    assert!(fetched.has_html);
    assert!(fetched.html_bytes > 0);
    assert!(fetched.body.contains("needle-history"));
    let rendered = serde_json::to_value(&fetched).unwrap();
    assert!(rendered.get("body_html").is_none(), "HTML 原文不进宿主 webview");
    assert!(!rendered.to_string().contains("<script>"));
    assert_eq!(rendered["remote_content"]["decision"], "block");

    // 拉回后本地已有：getMessage 命中，再次拉取幂等（走本地缓存）。
    let local = bridge.get_message("legacy", "INBOX", "1").await.unwrap();
    assert_eq!(local.summary.subject, "三年前的发票");
    let again = bridge
        .fetch_server_message("legacy".into(), "INBOX".into(), "1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.summary.uid, "1");
    // 服务器上也不存在 → None；非法 UID 在桥内挡下。
    assert_eq!(
        bridge
            .fetch_server_message("legacy".into(), "INBOX".into(), "9999".into())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        bridge
            .fetch_server_message("legacy".into(), "INBOX".into(), "abc".into())
            .await
            .unwrap_err()
            .code,
        UiErrorCode::Invalid
    );

    let documents = DocumentStore::new();
    let base = ProtocolBase::for_platform();
    let document = bridge
        .prepare_document(&documents, &base, "legacy", "INBOX", "1", false)
        .await
        .unwrap();
    assert!(document.has_html);
    let token = document.token.expect("有 HTML 就有令牌");
    assert!(document.url.unwrap().contains(&token));
    assert_eq!(document.remote_refs, 1, "远程图片被识别并默认阻止");
    assert_eq!(document.links, 1);
    let stored = documents.get(&token).expect("文档按令牌可取");
    assert!(stored.html.contains("needle-history"));
    assert!(!stored.html.contains("alert(1)"), "脚本被去势");
    assert!(
        !stored.html.contains(" src=\"https://"),
        "远程引用默认被去势（只留 data-maild-src 供用户放行）: {}",
        stored.html
    );

    // 4. 低占用观测与规则字段经同一前端主体可读。
    let status = bridge.raw("status", json!({})).await.unwrap();
    let runtime = &status["runtime"];
    assert!(runtime.is_object(), "{status}");
    assert!(runtime["stored_messages"].as_u64().unwrap() >= 4);
    assert!(runtime["store_db_bytes"].as_u64().unwrap() > 0);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    assert!(runtime["rss_bytes"].as_u64().unwrap() > 0);
    let rules = bridge.settings_rules(None).await.unwrap();
    assert_eq!(rules.rules.notify_new_mail, Some(true));
    assert_eq!(rules.rules.muted_accounts, Some(Vec::new()));
    assert_eq!(rules.persisted, Some(false), "未开 persist 时规则只在内存");
    let mut patch = rules.rules.clone();
    patch.notify_new_mail = Some(false);
    patch.muted_accounts = Some(vec!["legacy".into()]);
    let saved = bridge.settings_rules(Some(patch.clone())).await.unwrap();
    assert_eq!(saved.rules, patch, "关掉开关也能落到后台，不被缺省不序列化吞掉");
    assert_eq!(bridge.settings_rules(None).await.unwrap().rules, patch);

    bridge.shutdown().await;
    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}

/// `setSettings` 的 `persist: true`：守护进程有受管配置目录时运行时挂载 `rules_state.toml`，
/// 重启后仍在；与 ADR-0043 的覆盖层语义一致（只记用户改过的字段）。
#[tokio::test(flavor = "current_thread")]
async fn set_settings_can_opt_into_sidecar_persistence_at_runtime() {
    let server = fake_mail::FakeMail::start().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let paths = test_paths(directory.path());
    write_config(&paths, server.imap_addr.port());
    let socket = paths.runtime_dir.join("mail.sock");
    let sidecar = paths.config_dir.join("rules_state.toml");

    let (mut task, shutdown) = spawn_daemon(&paths);
    wait_socket_or_fail(&mut task, &socket).await;
    let client = maild_client::RpcClient::new(Endpoint::Unix {
        path: socket.clone(),
    })
    .unwrap();
    let before = client.call("getSettings", json!({})).await.unwrap();
    assert_eq!(before["persisted"], false);
    let ephemeral = client
        .call(
            "setSettings",
            json!({ "rules": { "silent_keywords": ["周报"] } }),
        )
        .await
        .unwrap();
    assert_eq!(ephemeral["persisted"], false);
    assert!(!sidecar.exists(), "未请求持久化时不制造状态文件");

    let persisted = client
        .call(
            "setSettings",
            json!({ "rules": { "muted_accounts": ["legacy"] }, "persist": true }),
        )
        .await
        .unwrap();
    assert_eq!(persisted["persisted"], true);
    assert_eq!(persisted["rules"]["silent_keywords"], json!(["周报"]));
    assert!(sidecar.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&sidecar).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let text = std::fs::read_to_string(&sidecar).unwrap();
    assert!(text.contains("周报") && text.contains("legacy"), "{text}");
    let typed = client.settings().await.unwrap();
    assert_eq!(typed.persisted, Some(true));
    assert_eq!(typed.rules.muted_accounts, ["legacy"]);
    let error = client
        .call("setSettings", json!({ "rules": {}, "persist": "yes" }))
        .await
        .unwrap_err();
    assert_eq!(error.rpc_code, Some(-32602));

    let _ = shutdown.send(());
    task.await.unwrap().unwrap();

    // 重启：TOML 没开 persist，但侧车已存在且经运行时请求挂载过——需要再次请求才挂载。
    // 再次 `persist: true` 读回磁盘覆盖层，用户改过的字段都在。
    let (mut task, shutdown) = spawn_daemon(&paths);
    wait_socket_or_fail(&mut task, &socket).await;
    let client = maild_client::RpcClient::new(Endpoint::Unix { path: socket }).unwrap();
    let reopened = client
        .call("setSettings", json!({ "rules": {}, "persist": true }))
        .await
        .unwrap();
    assert_eq!(reopened["persisted"], true);
    assert_eq!(reopened["rules"]["silent_keywords"], json!(["周报"]));
    assert_eq!(reopened["rules"]["muted_accounts"], json!(["legacy"]));
    let _ = shutdown.send(());
    task.await.unwrap().unwrap();
}
