//! ADR-0041 低占用后台收信验收：冷启动有界窗口 + 分批抓取、按需单封拉取、
//! 「立即同步」打断 IDLE 与断线退避、store 占用观测与进程内存读数。
//!
//! 全部 hermetic：本地假 IMAP 服务器 / 本地 TCP 监听 / 临时目录。

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::fake_mail::{FakeMail, ListReply};
use cordis::Context;
use maild::api::{dispatch, JsonRpcRequest};
use maild::events::{AccountStateKind, MailAttachment, MailMessage};
use maild::notify::Notifier;
use maild::receive::{Receive, FETCH_BATCH_SIZE};
use maild::store::Store;
use maild::Accounts;
use serde_json::{json, Value};

async fn wait_until<F: Fn() -> bool>(what: &str, deadline: Duration, check: F) {
    let until = tokio::time::Instant::now() + deadline;
    while tokio::time::Instant::now() < until {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("超时: {what}");
}

fn raw_mail(subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: sender@test.local\r\nTo: rcpt@test.local\r\n\
         Subject: {subject}\r\nDate: Tue, 1 Mar 2022 10:00:00 +0000\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

fn assembly_toml(port: u16, extra: &str) -> String {
    format!(
        r#"
[plugins.store]
[plugins.store.config]
db_path = ""

[[accounts]]
account = "e2e"
server = "127.0.0.1"
port = {port}
username = "dev"
password = "dev"
{extra}
"#
    )
}

fn receive_of(ctx: &Context) -> Receive {
    ctx.get::<Accounts>()
        .expect("accounts 服务")
        .snapshot()
        .swap_remove(0)
        .2
}

/// 某条 FETCH 的 UID 集合覆盖到的最小 UID（`N:*` 取 N，逗号清单取最小值）。
fn lowest_uid_in(range: &str) -> u64 {
    if let Some((start, _)) = range.split_once(':') {
        return start.parse().unwrap_or(0);
    }
    range
        .split(',')
        .filter_map(|value| value.parse().ok())
        .min()
        .unwrap_or(0)
}

#[tokio::test(flavor = "current_thread")]
async fn cold_start_syncs_only_the_newest_window_in_batches_then_fetches_history_on_demand() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    for index in 1..=500 {
        server.deliver_to(
            "INBOX",
            "",
            raw_mail(&format!("历史 {index}"), &format!("body-{index}")),
        );
    }

    let ctx = Context::new();
    let mut assembly = maild::assemble(
        &ctx,
        &assembly_toml(server.imap_addr.port(), "initial_sync_limit = 50"),
    )
    .expect("装配成功");
    assert!(assembly.errors.is_empty(), "装配错误: {:?}", assembly.errors);
    let receive = receive_of(&ctx);
    tokio::time::timeout(Duration::from_secs(10), receive.wait_online())
        .await
        .expect("10s 内未在线");

    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    wait_until("冷启动窗口落库", Duration::from_secs(15), || {
        probe.count_messages(Some("e2e"), Some("INBOX")).unwrap() == 50
    })
    .await;
    let probe = store.clone();
    wait_until("游标推进到 500", Duration::from_secs(10), || {
        probe
            .folder_by_name("e2e", "INBOX")
            .unwrap()
            .and_then(|folder| folder.last_seen_uid)
            == Some(500)
    })
    .await;

    // 恰好是最新的 50 封：UID 451..=500。
    let stored: BTreeSet<u64> = store
        .list_message_summaries(Some("e2e"), Some("INBOX"), 1000, 0)
        .unwrap()
        .into_iter()
        .map(|summary| summary.uid.parse().unwrap())
        .collect();
    assert_eq!(stored, (451..=500).collect::<BTreeSet<u64>>());

    // 前 450 封的正文从未被请求过，且每条正文 FETCH 不超过一批。
    let bodies: Vec<(String, String, String)> = server
        .fetch_log()
        .into_iter()
        .filter(|(_, _, items)| items.to_ascii_uppercase().contains("BODY"))
        .collect();
    assert!(!bodies.is_empty(), "至少发生过一次正文抓取");
    for (_, range, _) in &bodies {
        assert!(
            lowest_uid_in(range) >= 451,
            "冷启动不得拉取窗口之外的正文: {range}"
        );
        assert!(
            range.contains(':') || range.split(',').count() <= FETCH_BATCH_SIZE,
            "正文抓取必须分批: {range}"
        );
        assert_ne!(range, "1:*", "冷启动不得整箱拉正文");
    }

    // 新邮件照常经 IDLE 实时落库。假服务器只在 IDLE 期间推送 EXISTS：先让循环
    // 走完对账、挂上 IDLE。
    tokio::time::sleep(Duration::from_millis(500)).await;
    server.deliver_to("INBOX", "", raw_mail("新到 501", "实时"));
    let probe = store.clone();
    wait_until("第 501 封经 IDLE 落库", Duration::from_secs(10), || {
        probe
            .get_message("e2e", Some("INBOX"), "501")
            .unwrap()
            .is_some()
    })
    .await;
    assert_eq!(store.count_messages(Some("e2e"), Some("INBOX")).unwrap(), 51);

    // 按需拉回窗口之外的历史邮件，并持久化。
    assert!(store.get_message("e2e", Some("INBOX"), "10").unwrap().is_none());
    let fetched = tokio::time::timeout(
        Duration::from_secs(10),
        receive.fetch_message_by_uid("INBOX", 10),
    )
    .await
    .expect("按需拉取未在 10s 内返回")
    .expect("按需拉取成功")
    .expect("UID 10 存在");
    assert_eq!(fetched.uid, "10");
    assert_eq!(fetched.subject, "历史 10");
    assert_eq!(fetched.folder, "INBOX");
    assert_eq!(fetched.folder_role, "inbox");
    let persisted = store
        .get_message("e2e", Some("INBOX"), "10")
        .unwrap()
        .expect("按需拉取的邮件已落库");
    assert_eq!(persisted.subject, "历史 10");
    assert!(persisted.body.contains("body-10"));

    // 幂等：再拉一次不产生重复行。
    receive
        .fetch_message_by_uid("INBOX", 10)
        .await
        .expect("再次拉取成功")
        .expect("仍然存在");
    assert_eq!(store.count_messages(Some("e2e"), Some("INBOX")).unwrap(), 52);

    // 服务器上不存在的 UID 不是错误。
    let missing = receive
        .fetch_message_by_uid("INBOX", 9_999)
        .await
        .expect("不存在的 UID 不是错误");
    assert!(missing.is_none());

    // 按需拉取走收信那条连接，不另开（ADR-0026 连接预算 = 1）。
    assert_eq!(server.imap_connections(), 1, "按需拉取不得新建连接");

    // 窗口截断后游标不回退：后续同步只请求 501 之后。
    let cursor = store
        .folder_by_name("e2e", "INBOX")
        .unwrap()
        .and_then(|folder| folder.last_seen_uid);
    assert_eq!(cursor, Some(501));

    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn small_mailboxes_keep_the_full_cold_sync() {
    // 不超过窗口的信箱：行为与 v4 逐封一致（整箱一条 `1:*` 流式抓取）。
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    for index in 1..=3 {
        server.deliver_to("INBOX", "", raw_mail(&format!("小信箱 {index}"), "x"));
    }
    let ctx = Context::new();
    let mut assembly = maild::assemble(
        &ctx,
        &assembly_toml(server.imap_addr.port(), "initial_sync_limit = 50"),
    )
    .expect("装配成功");
    let receive = receive_of(&ctx);
    tokio::time::timeout(Duration::from_secs(10), receive.wait_online())
        .await
        .expect("10s 内未在线");
    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    wait_until("三封全部落库", Duration::from_secs(10), || {
        probe.count_messages(Some("e2e"), Some("INBOX")).unwrap() == 3
    })
    .await;
    let first_body = server
        .fetch_log()
        .into_iter()
        .find(|(_, _, items)| items.to_ascii_uppercase().contains("BODY"))
        .expect("发生过正文抓取");
    assert_eq!(first_body.1, "1:*");
    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_sync_wakes_an_idling_loop_and_sweeps_every_folder() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    server.add_mailbox("Archive", "\\Archive");

    let ctx = Context::new();
    // sweep 周期拉到 1 小时：测试期间只有 trigger_sync 能让 Archive 被再次同步。
    let mut assembly = maild::assemble(
        &ctx,
        &assembly_toml(server.imap_addr.port(), "folder_poll_secs = 3600"),
    )
    .expect("装配成功");
    let receive = receive_of(&ctx);
    tokio::time::timeout(Duration::from_secs(10), receive.wait_online())
        .await
        .expect("10s 内未在线");
    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    wait_until("首轮 sweep 完成", Duration::from_secs(10), || {
        probe
            .folder_by_name("e2e", "Archive")
            .unwrap()
            .is_some_and(|folder| folder.synced_at.is_some())
    })
    .await;
    // 给循环一点时间挂上 IDLE。
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 非主文件夹的投递不会触发 IDLE 推送。
    server.deliver_to("Archive", "", raw_mail("归档新信", "需要立即同步"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        store.count_messages(Some("e2e"), Some("Archive")).unwrap(),
        0,
        "没有 trigger_sync 时 Archive 要等下一次 sweep"
    );

    receive.trigger_sync();
    let probe = store.clone();
    wait_until("trigger_sync 后 Archive 落库", Duration::from_secs(5), || {
        probe.count_messages(Some("e2e"), Some("Archive")).unwrap() == 1
    })
    .await;
    assert_eq!(server.imap_connections(), 1, "立即同步复用收信连接");
    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn manual_sync_refreshes_folders_on_idle_and_poll_connections() {
    for extra in [
        "folder_poll_secs = 3600",
        "idle = false\npoll_secs = 3600\nfolder_poll_secs = 3600\n[plugins.scheduler]",
    ] {
        let server = FakeMail::start().await.expect("假邮件服务器启动");
        server.add_mailbox("Archive", "\\Archive");
        server.deliver_to("Archive", "", raw_mail("旧邮件", "x"));
        let ctx = Context::new();
        let mut assembly = maild::assemble(&ctx, &assembly_toml(server.imap_addr.port(), extra))
            .expect("装配成功");
        let receive = receive_of(&ctx);
        tokio::time::timeout(Duration::from_secs(10), receive.wait_online())
            .await
            .expect("10s 内未在线");
        let store = ctx.get::<Store>().expect("store 服务");

        tracked_sync(&ctx, Some("Archive")).await;
        assert_eq!(
            store.count_messages(Some("e2e"), Some("Archive")).unwrap(),
            1
        );

        server.remove_mailbox("Archive");
        server.add_mailbox("Renamed", "\\Archive");
        server.add_mailbox("Projects", "");
        server.deliver_to("Renamed", "", raw_mail("新归档邮件", "x"));
        server.deliver_to("Projects", "", raw_mail("新项目邮件", "x"));

        // The item the user right-clicked may have vanished before LIST runs.
        tracked_sync(&ctx, Some("Archive")).await;
        assert!(
            !store
                .folder_by_name("e2e", "Archive")
                .unwrap()
                .unwrap()
                .present
        );
        assert!(
            store
                .folder_by_name("e2e", "Renamed")
                .unwrap()
                .unwrap()
                .present
        );
        assert_eq!(
            store.count_messages(Some("e2e"), Some("Archive")).unwrap(),
            1
        );
        assert_eq!(
            store.count_messages(Some("e2e"), Some("Renamed")).unwrap(),
            0
        );

        tracked_sync(&ctx, Some("Renamed")).await;
        assert_eq!(
            store.count_messages(Some("e2e"), Some("Renamed")).unwrap(),
            1
        );
        assert_eq!(
            store.count_messages(Some("e2e"), Some("Projects")).unwrap(),
            0
        );

        server.add_mailbox("New", "");
        server.deliver_to("New", "", raw_mail("新文件夹邮件", "x"));
        tracked_sync(&ctx, None).await;
        assert!(store.folder_by_name("e2e", "New").unwrap().unwrap().present);
        assert_eq!(store.count_messages(Some("e2e"), Some("New")).unwrap(), 1);
        assert_eq!(
            store.count_messages(Some("e2e"), Some("Projects")).unwrap(),
            1
        );
        assert_eq!(server.imap_connections(), 1);
        assembly.dispose().await;
    }
}

async fn tracked_sync(ctx: &Context, folder: Option<&str>) {
    let started = sync_call(
        ctx,
        json!({"account": "e2e", "folder": folder, "track": true}),
    )
    .await;
    let id = started["sync_id"].as_u64().expect("同步 ID");
    wait_for_tracked_sync(ctx, id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn manual_sync_rejects_failed_and_incomplete_folder_lists() {
    let server = FakeMail::start().await.unwrap();
    server.add_mailbox("Archive", "");
    server.deliver_to("Archive", "", raw_mail("Cached mail", "body"));
    let ctx = Context::new();
    let mut assembly =
        maild::assemble(&ctx, &assembly_toml(server.imap_addr.port(), "")).expect("装配成功");
    let store = ctx.get::<Store>().expect("store 服务");
    tokio::time::timeout(Duration::from_secs(5), receive_of(&ctx).wait_online())
        .await
        .unwrap();
    tracked_sync(&ctx, Some("Archive")).await;
    server.remove_mailbox("Archive");
    server.add_mailbox("Projects", "\\NoSelect");

    for (reply, expected_error) in [
        (ListReply::Rejected, "Fixture LIST rejected"),
        (ListReply::PartialRejected, "Fixture LIST rejected"),
        (ListReply::Empty, "LIST 未返回可选文件夹"),
        (ListReply::WithoutInbox, "LIST 未返回可选文件夹"),
    ] {
        server.set_list_reply(reply);
        let started = sync_call(&ctx, json!({"account": "e2e", "track": true})).await;
        let id = started["sync_id"].as_u64().unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let response = dispatch(
                    &ctx,
                    &JsonRpcRequest {
                        jsonrpc: "2.0".into(),
                        method: "syncMessages".into(),
                        params: json!({"account": "e2e", "sync_id": id}),
                        id: json!(1),
                    },
                )
                .await;
                if let Some(error) = response.error {
                    break error;
                }
                assert_eq!(response.result.unwrap()["done"], false);
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert!(error.message.contains(expected_error), "{error:?}");
        for folder in ["INBOX", "Archive"] {
            assert!(
                store
                    .folder_by_name("e2e", folder)
                    .unwrap()
                    .unwrap()
                    .present
            );
        }
        assert!(store.folder_by_name("e2e", "Projects").unwrap().is_none());
        assert_eq!(
            store.count_messages(Some("e2e"), Some("Archive")).unwrap(),
            1
        );
    }

    server.set_list_reply(ListReply::Normal);
    tracked_sync(&ctx, None).await;
    assert!(
        !store
            .folder_by_name("e2e", "Archive")
            .unwrap()
            .unwrap()
            .present
    );
    assert_eq!(server.imap_connections(), 1);
    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn successful_list_without_inbox_syncs_selectable_folders() {
    let server = FakeMail::start().await.unwrap();
    server.set_list_reply(ListReply::WithoutInbox);
    server.add_mailbox("Archive", "");
    server.deliver_to("Archive", "", raw_mail("Startup mail", "body"));
    let ctx = Context::new();
    let config = assembly_toml(
        server.imap_addr.port(),
        r#"
[plugins.filters]
[plugins.filters.config]
[[plugins.filters.config.rules]]
field = "subject"
pattern = "mail"
action = "drop"
"#,
    );
    let mut assembly = maild::assemble(&ctx, &config).expect("装配成功");
    assert!(
        assembly.errors.is_empty(),
        "装配错误: {:?}",
        assembly.errors
    );
    let store = ctx.get::<Store>().expect("store 服务");
    wait_until("启动同步归档邮件", Duration::from_secs(5), || {
        store.count_messages(Some("e2e"), Some("Archive")).unwrap() == 1
    })
    .await;

    server.deliver_to("Archive", "", raw_mail("Manual sync mail", "body"));
    tracked_sync(&ctx, Some("Archive")).await;
    assert_eq!(
        store.count_messages(Some("e2e"), Some("Archive")).unwrap(),
        2
    );

    server.add_mailbox("Projects", "");
    server.deliver_to("Projects", "", raw_mail("New folder mail", "body"));
    tracked_sync(&ctx, None).await;
    assert_eq!(
        store.count_messages(Some("e2e"), Some("Projects")).unwrap(),
        1
    );
    assert_eq!(server.imap_connections(), 1);
    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn localized_inbox_without_inbox_runs_filters_and_notifies() {
    assert_localized_inbox_runs_filters_and_notifies(None).await;
}

#[tokio::test(flavor = "current_thread")]
async fn declared_inbox_wins_over_an_earlier_inbox_like_folder() {
    assert_localized_inbox_runs_filters_and_notifies(Some("Nested/INBOX")).await;
}

async fn assert_localized_inbox_runs_filters_and_notifies(guessed_inbox: Option<&str>) {
    let server = FakeMail::start().await.unwrap();
    server.set_list_reply(ListReply::WithoutInbox);
    server.add_mailbox("Archive", "\\Archive");
    let inbox = "Posteingang";
    if let Some(guessed_inbox) = guessed_inbox {
        server.add_mailbox(guessed_inbox, "");
        server.deliver_to(guessed_inbox, "", raw_mail("Drop this", "body"));
    }
    server.add_mailbox(inbox, "\\Inbox");
    for subject in ["Drop this", "Keep this"] {
        server.deliver_to(
            inbox,
            "",
            format!("From: sender@test.local\r\nSubject: {subject}\r\n\r\nbody\r\n").into_bytes(),
        );
    }

    let ctx = Context::new();
    let config = assembly_toml(
        server.imap_addr.port(),
        r#"
[plugins.filters]
[plugins.filters.config]
[[plugins.filters.config.rules]]
field = "subject"
pattern = "Drop"
action = "drop"

[plugins.notify]
[plugins.rules]
[plugins.rules.config]
notify_new_mail = true
"#,
    );
    let mut assembly = maild::assemble(&ctx, &config).expect("装配成功");
    assert!(assembly.errors.is_empty(), "装配错误: {:?}", assembly.errors);
    tracked_sync(&ctx, None).await;

    let folders = dispatch(
        &ctx,
        &JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "listFolders".into(),
            params: json!({ "account": "e2e" }),
            id: json!(1),
        },
    )
    .await
    .result
    .unwrap();
    let listed = folders["folders"].as_array().unwrap();
    let primary: Vec<_> = listed.iter().filter(|folder| folder["is_primary"] == true).collect();
    assert_eq!(primary.len(), 1);
    assert_eq!(primary[0]["name"], inbox);

    let store = ctx.get::<Store>().expect("store 服务");
    if let Some(guessed_inbox) = guessed_inbox {
        assert_eq!(
            store.count_messages(Some("e2e"), Some(guessed_inbox)).unwrap(),
            1
        );
    }
    let messages = store
        .list_message_summaries(Some("e2e"), Some(inbox), 10, 0)
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].subject, "Keep this");
    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    wait_until("收件箱新邮件通知", Duration::from_secs(5), || {
        !notifier.sent().is_empty()
    })
    .await;
    assert_eq!(notifier.sent().len(), 1);
    assert!(notifier.sent()[0].body.contains("Keep this"));
    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn connection_time_list_rejection_preserves_cached_folders() {
    let server = FakeMail::start().await.unwrap();
    server.set_list_reply(ListReply::PartialRejected);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("mail.db");
    let db = db.to_str().unwrap();
    {
        let store = Store::open(db).unwrap();
        store.upsert_folder("e2e", "INBOX", "INBOX", None).unwrap();
        store
            .upsert_folder("e2e", "Archive", "Archive", None)
            .unwrap();
    }
    let ctx = Context::new();
    let config = assembly_toml(server.imap_addr.port(), "")
        .replace(r#"db_path = """#, &format!("db_path = {db:?}"));
    let mut assembly = maild::assemble(&ctx, &config).expect("装配成功");
    tokio::time::timeout(Duration::from_secs(5), receive_of(&ctx).wait_online())
        .await
        .unwrap();
    let store = ctx.get::<Store>().expect("store 服务");
    assert!(
        store
            .folder_by_name("e2e", "Archive")
            .unwrap()
            .unwrap()
            .present
    );
    assembly.dispose().await;
}

async fn wait_for_tracked_sync(ctx: &Context, id: u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = sync_call(ctx, json!({"account": "e2e", "sync_id": id})).await;
            if status["done"] == true {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("同步超时");
}

async fn sync_call(ctx: &Context, params: Value) -> Value {
    let response = dispatch(
        ctx,
        &JsonRpcRequest {
            jsonrpc: "2.0".into(),
            method: "syncMessages".into(),
            params,
            id: json!(1),
        },
    )
    .await;
    response
        .result
        .unwrap_or_else(|| panic!("{:?}", response.error))
}

#[tokio::test(flavor = "current_thread")]
async fn trigger_sync_and_shutdown_cut_the_reconnect_backoff_short() {
    // 前两次连接失败在读 greeting，第三次转发给假 IMAP 服务器。
    let server = FakeMail::start().await.unwrap();
    let imap_addr = server.imap_addr;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepts);
    let acceptor = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            if counter.fetch_add(1, Ordering::SeqCst) >= 2 {
                let mut imap = tokio::net::TcpStream::connect(imap_addr).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut imap).await;
            }
        }
    });

    let ctx = Context::new();
    // 退避基数 1 小时：只有 trigger_sync / 关断能让循环提前醒来。
    let mut assembly =
        maild::assemble(&ctx, &assembly_toml(port, "reconnect_secs = 3600")).expect("装配成功");
    let receive = receive_of(&ctx);

    let counter = Arc::clone(&accepts);
    wait_until("首次连接尝试", Duration::from_secs(5), || {
        counter.load(Ordering::SeqCst) >= 1
    })
    .await;
    let probe = receive.clone();
    wait_until("进入离线退避", Duration::from_secs(5), || {
        probe.state().state == AccountStateKind::Offline
    })
    .await;
    assert!(!receive.is_online());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(accepts.load(Ordering::SeqCst), 1, "退避期间不得重连");

    // 离线时按需拉取立即给出明确错误，而不是挂到超时。
    let offline = receive.fetch_message_by_uid("INBOX", 1).await;
    assert!(offline.is_err(), "离线时应立即拒绝: {offline:?}");

    receive.trigger_sync();
    let counter = Arc::clone(&accepts);
    wait_until("trigger_sync 打断退避并重连", Duration::from_secs(5), || {
        counter.load(Ordering::SeqCst) >= 2
    })
    .await;

    wait_until("再次进入离线退避", Duration::from_secs(5), || {
        receive.state().state == AccountStateKind::Offline
    })
    .await;
    server.add_mailbox("Archive", "");
    let started = sync_call(
        &ctx,
        json!({"account": "e2e", "folder": "Archive", "track": true}),
    )
    .await;
    let id = started["sync_id"].as_u64().expect("同步 ID");
    wait_until(
        "跟踪同步打断退避并重连",
        Duration::from_secs(5),
        || accepts.load(Ordering::SeqCst) >= 3,
    )
    .await;

    wait_for_tracked_sync(&ctx, id).await;

    // 再断开进入退避，确认关断不必等待退避睡眠结束。
    acceptor.abort();
    wait_until(
        "转发断开后进入离线退避",
        Duration::from_secs(5),
        || receive.state().state == AccountStateKind::Offline,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), assembly.dispose())
        .await
        .expect("退避中的收信循环应立即响应关断");
}

fn sample_message(uid: &str, attachment: &[u8]) -> MailMessage {
    MailMessage {
        account: "acc".into(),
        uid: uid.into(),
        sender: "sender@example.test".into(),
        subject: format!("subject {uid}"),
        body: "body".into(),
        received_at: 1_700_000_000,
        folder: "INBOX".into(),
        has_attachments: true,
        attachments: vec![MailAttachment {
            part_index: 1,
            filename: "a.bin".into(),
            size: attachment.len() as u64,
            content: Some(attachment.to_vec()),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn store_metrics_and_shrink_memory_on_a_file_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("maild.db");
    let store = Store::open(path.to_str().unwrap()).unwrap();
    let empty = store.storage_metrics();
    assert_eq!(empty.message_count, 0);
    assert_eq!(empty.stored_attachment_bytes, 0);
    assert!(empty.db_bytes > 0, "库文件已创建: {empty:?}");

    let payload = vec![0x5a_u8; 64 * 1024];
    for uid in 1..=20 {
        assert!(store
            .insert_message(&sample_message(&uid.to_string(), &payload))
            .unwrap());
    }
    let filled = store.storage_metrics();
    assert_eq!(filled.message_count, 20);
    assert_eq!(filled.stored_attachment_bytes, 20 * payload.len() as u64);
    assert!(filled.db_bytes + filled.wal_bytes > 20 * payload.len() as u64);

    store.shrink_memory();
    let shrunk = store.storage_metrics();
    assert_eq!(shrunk.message_count, 20, "收紧内存不改内容");
    assert!(
        shrunk.wal_bytes <= 4 * 1024 * 1024,
        "checkpoint 后 WAL 受 journal_size_limit 约束: {shrunk:?}"
    );
    // 收紧之后库仍然完全可用。
    assert!(store.insert_message(&sample_message("21", b"tail")).unwrap());
    assert_eq!(store.storage_metrics().message_count, 21);
}

#[test]
fn store_metrics_on_an_in_memory_database() {
    let store = Store::open("").unwrap();
    store.insert_message(&sample_message("1", b"abc")).unwrap();
    store.shrink_memory();
    let metrics = store.storage_metrics();
    assert_eq!(metrics.message_count, 1);
    assert_eq!(metrics.stored_attachment_bytes, 3);
    assert_eq!(metrics.wal_bytes, 0);
    assert!(metrics.db_bytes > 0);
}

#[test]
fn process_memory_readout_and_release() {
    maild::daemon::release_unused_memory();
    let reading = maild::daemon::current_process_memory_bytes();
    if cfg!(target_os = "macos") {
        let (rss, footprint) = reading.expect("macOS 必须给出读数");
        assert!(rss > 0);
        assert!(footprint.expect("macOS 给出 phys_footprint") > 0);
    } else if cfg!(target_os = "linux") {
        let (rss, footprint) = reading.expect("Linux 给出 RSS");
        assert!(rss > 0);
        assert!(footprint.is_none());
    } else {
        assert!(reading.is_none());
    }
}
