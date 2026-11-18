//! M4.5-02 文件夹与增量同步验收（ADR-0026）。
//!
//! 前半是 store 层（快，不联网）：v4 的身份维度、游标、重建、过滤与清单。
//! 后半是 IMAP 层，跑本地假服务器：LIST 发现、role 映射、增量抓取、
//! `UIDVALIDITY` 翻转重建、非收件箱不触发通知。

mod common;

use std::time::Duration;

use common::fake_mail::FakeMail;
use cordis::Context;
use maild::events::{MailAttachment, MailMessage};
use maild::folders::FolderRole;
use maild::notify::Notifier;
use maild::store::Store;
use maild::Accounts;

fn message(account: &str, folder: &str, uid: &str, subject: &str) -> MailMessage {
    MailMessage {
        account: account.into(),
        uid: uid.into(),
        sender: "sender@example.test".into(),
        subject: subject.into(),
        body: "body".into(),
        received_at: 1_700_000_000,
        folder: folder.into(),
        ..Default::default()
    }
}

// —— store 层：身份维度与游标 ——

#[test]
fn same_uid_in_two_folders_is_two_messages() {
    // v3 的 (account, uid) 唯一键会把收件箱的 UID 42 和已发送的 UID 42 当成同一封。
    let store = Store::open("").unwrap();
    assert!(store.insert_message(&message("acc", "INBOX", "42", "收件")).unwrap());
    assert!(store.insert_message(&message("acc", "Sent", "42", "已发")).unwrap());
    assert_eq!(store.count().unwrap(), 2);

    // 同一 (account, folder, uid) 仍然幂等。
    assert!(!store.insert_message(&message("acc", "Sent", "42", "重放")).unwrap());
    assert_eq!(store.count().unwrap(), 2);

    // 省略 folder 时按固定规则消歧：收件箱优先。
    let ambiguous = store.get_message("acc", None, "42").unwrap().unwrap();
    assert_eq!(ambiguous.folder, "INBOX");
    let explicit = store.get_message("acc", Some("Sent"), "42").unwrap().unwrap();
    assert_eq!(explicit.subject, "已发");
    assert!(store.get_message("acc", Some("Drafts"), "42").unwrap().is_none());
    let summaries = store
        .list_message_summaries(Some("acc"), Some("Sent"), 10, 0)
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].subject, "已发");
    assert_eq!(summaries[0].folder, "Sent");
}

#[test]
fn attachments_are_scoped_to_the_folder_too() {
    let store = Store::open("").unwrap();
    let attachment = |name: &str| MailAttachment {
        part_index: 1,
        filename: name.into(),
        content_id: "cid@maild".into(),
        size: 4,
        content: Some(name.as_bytes().to_vec()),
        ..Default::default()
    };
    let mut inbox = message("acc", "INBOX", "42", "收件");
    inbox.attachments = vec![attachment("inbox.bin")];
    let mut sent = message("acc", "Sent", "42", "已发");
    sent.attachments = vec![attachment("sent.bin")];
    store.insert_message(&inbox).unwrap();
    store.insert_message(&sent).unwrap();

    assert_eq!(
        store.list_attachments("acc", "INBOX", "42").unwrap()[0].filename,
        "inbox.bin"
    );
    assert_eq!(
        store.list_attachments("acc", "Sent", "42").unwrap()[0].filename,
        "sent.bin",
        "同号 UID 的附件不得互相顶掉"
    );
    let by_cid = store
        .attachment_by_content_id("acc", "Sent", "42", "cid@maild")
        .unwrap()
        .unwrap();
    assert_eq!(by_cid.content.as_deref(), Some(b"sent.bin".as_slice()));
}

#[test]
fn omitting_the_folder_filter_keeps_the_old_behaviour() {
    let store = Store::open("").unwrap();
    store.insert_message(&message("acc", "INBOX", "1", "a")).unwrap();
    store.insert_message(&message("acc", "Archive", "1", "b")).unwrap();
    assert_eq!(store.count_messages(Some("acc"), None).unwrap(), 2);
    assert_eq!(
        store.list_message_summaries(Some("acc"), None, 10, 0).unwrap().len(),
        2
    );
    assert_eq!(store.count_messages(Some("acc"), Some("Archive")).unwrap(), 1);
    assert_eq!(store.count_messages(None, Some("不存在")).unwrap(), 0);
}

#[test]
fn folder_listing_carries_role_counts_and_presence() {
    let store = Store::open("").unwrap();
    let inbox = store
        .upsert_folder("acc", "INBOX", "INBOX", Some(FolderRole::Inbox))
        .unwrap();
    store
        .upsert_folder("acc", "&XfJT0ZAB-", "已发送", Some(FolderRole::Sent))
        .unwrap();
    store.upsert_folder("acc", "客户", "客户", None).unwrap();

    let mut unread = message("acc", "INBOX", "1", "未读");
    unread.seen = false;
    let mut read = message("acc", "INBOX", "2", "已读");
    read.seen = true;
    store.insert_message(&unread).unwrap();
    store.insert_message(&read).unwrap();
    assert_eq!(store.folder_counts(inbox).unwrap(), (2, 1));

    // 只标记缺席，不删除：LIST 是清单的权威，不是删除邮件的授权。
    let marked = store
        .mark_absent_folders("acc", &["INBOX".into(), "&XfJT0ZAB-".into()])
        .unwrap();
    assert_eq!(marked, 1);
    let listed = store.list_folders(Some("acc")).unwrap();
    assert_eq!(listed.len(), 3);
    let custom = listed.iter().find(|f| f.name == "客户").unwrap();
    assert!(!custom.present);
    assert_eq!(custom.role, None, "自建文件夹不该被猜角色");
    let sent = listed.iter().find(|f| f.role == Some(FolderRole::Sent)).unwrap();
    assert_eq!(sent.display_name, "已发送");
    assert!(sent.present);
}

#[test]
fn uid_validity_flip_rebuilds_the_folder_and_clears_the_cursor() {
    let store = Store::open("").unwrap();
    let folder = store
        .upsert_folder("acc", "INBOX", "INBOX", Some(FolderRole::Inbox))
        .unwrap();
    let mut with_attachment = message("acc", "INBOX", "7", "旧");
    with_attachment.attachments = vec![MailAttachment {
        part_index: 1,
        filename: "old.bin".into(),
        size: 3,
        content: Some(b"old".to_vec()),
        ..Default::default()
    }];
    store.insert_message(&with_attachment).unwrap();
    store.advance_folder_cursor(folder, 1, Some(7), 100).unwrap();

    let removed = store.rebuild_folder(folder, 2).unwrap();
    assert_eq!(removed, 1);
    assert_eq!(store.count().unwrap(), 0);
    assert!(
        store.list_attachments("acc", "INBOX", "7").unwrap().is_empty(),
        "附件必须随消息一起删除"
    );
    let record = store.folder_by_name("acc", "INBOX").unwrap().unwrap();
    assert_eq!(record.uid_validity, Some(2));
    assert_eq!(record.last_seen_uid, None, "游标清零，下次全量重建");
}

#[test]
fn cursor_never_moves_backwards() {
    let store = Store::open("").unwrap();
    let folder = store.upsert_folder("acc", "INBOX", "INBOX", None).unwrap();
    store.advance_folder_cursor(folder, 1, Some(9), 1).unwrap();
    // 一轮同步没有新消息时传回的是旧值，不能把游标拉回去。
    store.advance_folder_cursor(folder, 1, Some(4), 2).unwrap();
    store.advance_folder_cursor(folder, 1, None, 3).unwrap();
    let record = store.folder_by_name("acc", "INBOX").unwrap().unwrap();
    assert_eq!(record.last_seen_uid, Some(9));
    assert_eq!(record.synced_at, Some(3));
}

#[test]
fn upsert_corrects_a_role_guessed_before_list_ran() {
    let store = Store::open("").unwrap();
    // 落库路径先隐式建了文件夹（名字猜不出角色）……
    store.insert_message(&message("acc", "Bo&Cgt5-te", "1", "x")).unwrap();
    let before = store.folder_by_name("acc", "Bo&Cgt5-te").unwrap().unwrap();
    assert_eq!(before.role, None);
    // ……LIST 之后由服务器的明示声明纠正。
    store
        .upsert_folder("acc", "Bo&Cgt5-te", "Boîte", Some(FolderRole::Archive))
        .unwrap();
    let after = store.folder_by_name("acc", "Bo&Cgt5-te").unwrap().unwrap();
    assert_eq!(after.id, before.id, "纠正是更新而不是新建一行");
    assert_eq!(after.role, Some(FolderRole::Archive));
    assert_eq!(after.display_name, "Boîte");
    assert_eq!(store.count_messages(Some("acc"), Some("Bo&Cgt5-te")).unwrap(), 1);
}

// —— IMAP 层 ——

async fn wait_until<F: Fn() -> bool>(what: &str, deadline: Duration, check: F) {
    let until = tokio::time::Instant::now() + deadline;
    while tokio::time::Instant::now() < until {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
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

fn assembly_toml(port: u16) -> String {
    format!(
        r#"
[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "e2e"
server = "127.0.0.1"
port = {port}
username = "dev"
password = "dev"
folder_poll_secs = 30
"#
    )
}

#[tokio::test(flavor = "current_thread")]
async fn folders_are_discovered_and_only_the_inbox_notifies() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    server.add_mailbox("Sent", "\\Sent");
    server.add_mailbox("&XfJT0ZAB-", "");
    server.add_mailbox("&W6JiNw-", ""); // 「客户」的 modified UTF-7
    // 已发送里的历史邮件：必须落库，但绝不能触发通知或 OTP。
    server.deliver_to("Sent", "\\Seen", raw_mail("已发送的信", "你的验证码是 246810"));
    server.deliver_to(
        "&XfJT0ZAB-",
        "",
        raw_mail("中文文件夹", "你的验证码是 135790"),
    );

    let ctx = Context::new();
    let mut assembly = maild::assemble(&ctx, &assembly_toml(server.imap_addr.port()))
        .expect("装配成功");
    assert!(assembly.errors.is_empty(), "装配错误: {:?}", assembly.errors);

    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(
        Duration::from_secs(10),
        accounts.snapshot()[0].2.wait_online(),
    )
    .await
    .expect("10s 内未在线");

    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    wait_until("非收件箱邮件落库", Duration::from_secs(10), || {
        probe.count_messages(Some("e2e"), Some("Sent")).unwrap() == 1
            && probe
                .count_messages(Some("e2e"), Some("&XfJT0ZAB-"))
                .unwrap()
                == 1
    })
    .await;

    // role 判定：SPECIAL-USE 优先，名字兜底，猜不出即为空。
    let listed = store.list_folders(Some("e2e")).unwrap();
    let by_name = |name: &str| listed.iter().find(|f| f.name == name).expect(name);
    assert_eq!(by_name("INBOX").role, Some(FolderRole::Inbox));
    assert_eq!(by_name("Sent").role, Some(FolderRole::Sent));
    assert_eq!(by_name("&XfJT0ZAB-").display_name, "已发送");
    assert_eq!(
        by_name("&XfJT0ZAB-").role,
        Some(FolderRole::Sent),
        "名字兜底认得解码后的中文名"
    );
    let custom = by_name("&W6JiNw-");
    assert_eq!(custom.display_name, "客户");
    assert_eq!(custom.role, None, "用户自建文件夹不该被猜角色");
    assert!(listed.iter().all(|folder| folder.present));

    // 决策 9：非收件箱不进过滤/OTP/通知管线。
    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        notifier.sent().is_empty(),
        "已发送与归档里的历史邮件不得触发通知: {:?}",
        notifier.sent()
    );

    let sent = store
        .list_message_summaries(Some("e2e"), Some("Sent"), 10, 0)
        .unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].folder_role, "sent");
    assert!(sent[0].seen, "IMAP FLAGS 在首次抓取时快照");

    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn second_sync_asks_only_for_uids_after_the_cursor() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    server.deliver_to("INBOX", "", raw_mail("第一封", "hello"));

    let ctx = Context::new();
    let mut assembly = maild::assemble(&ctx, &assembly_toml(server.imap_addr.port()))
        .expect("装配成功");
    assert!(assembly.errors.is_empty(), "装配错误: {:?}", assembly.errors);
    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(
        Duration::from_secs(10),
        accounts.snapshot()[0].2.wait_online(),
    )
    .await
    .expect("10s 内未在线");

    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    wait_until("首封落库", Duration::from_secs(10), || {
        probe.count_messages(Some("e2e"), None).unwrap() == 1
    })
    .await;

    server.deliver_to("INBOX", "", raw_mail("第二封", "world"));
    let probe = store.clone();
    wait_until("第二封落库", Duration::from_secs(10), || {
        probe.count_messages(Some("e2e"), None).unwrap() == 2
    })
    .await;

    // 正文抓取与标记对账走同一条 FETCH 命令，靠请求的 items 区分。
    let log = server.fetch_log();
    let bodies: Vec<&(String, String, String)> = log
        .iter()
        .filter(|(_, _, items)| items.to_ascii_uppercase().contains("BODY"))
        .collect();
    assert!(!bodies.is_empty(), "至少发生过一次正文抓取");
    assert_eq!(bodies[0].1, "1:*", "首次同步是全量");
    assert!(
        bodies.iter().skip(1).all(|(_, range, _)| range != "1:*"),
        "游标之后不得再拉全量正文: {log:?}"
    );
    assert!(
        bodies.iter().any(|(_, range, _)| range == "2:*"),
        "第二次应只请求游标之后的 UID: {log:?}"
    );
    // 对账是 O(mailbox)，每个文件夹每次会话只做一次。
    let reconciles = log
        .iter()
        .filter(|(_, _, items)| !items.to_ascii_uppercase().contains("BODY"))
        .count();
    assert_eq!(reconciles, 1, "标记对账每次会话只做一次: {log:?}");
    // `N:*` 会带回最大 UID 那一条；客户端必须自行过滤，不能重复落库。
    assert_eq!(store.count_messages(Some("e2e"), None).unwrap(), 2);

    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn uid_validity_flip_rebuilds_without_duplicating_rows() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    server.add_mailbox("Archive", "\\Archive");
    server.deliver_to("Archive", "", raw_mail("归档旧信", "old"));

    let ctx = Context::new();
    let mut assembly = maild::assemble(&ctx, &assembly_toml(server.imap_addr.port()))
        .expect("装配成功");
    assert!(assembly.errors.is_empty(), "装配错误: {:?}", assembly.errors);
    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(
        Duration::from_secs(10),
        accounts.snapshot()[0].2.wait_online(),
    )
    .await
    .expect("10s 内未在线");

    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    wait_until("归档首次同步", Duration::from_secs(10), || {
        probe.count_messages(Some("e2e"), Some("Archive")).unwrap() == 1
    })
    .await;
    assembly.dispose().await;

    // 服务器侧 UID 空间重编号：同一个 UID 现在指向另一封信。
    server.set_uid_validity("Archive", 42);

    let ctx2 = Context::new();
    ctx2.provide::<Store>(store.clone());
    let folder = store.folder_by_name("e2e", "Archive").unwrap().unwrap();
    assert_eq!(folder.uid_validity, Some(1));
    let removed = store.rebuild_folder(folder.id, 42).unwrap();
    assert_eq!(removed, 1, "旧 UID 与新 UID 没有对应关系，只能整folder重建");
    assert_eq!(store.count_messages(Some("e2e"), Some("Archive")).unwrap(), 0);
    let after = store.folder_by_name("e2e", "Archive").unwrap().unwrap();
    assert_eq!(after.uid_validity, Some(42));
    assert_eq!(after.last_seen_uid, None);
}
