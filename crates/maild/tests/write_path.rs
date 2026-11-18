//! M4.5-05 写路径与离线队列验收（ADR-0027）。
//!
//! 前半是 store 层的状态机与对账护栏（快，不联网）；后半跑本地假服务器，
//! 覆盖 `MOVE` 与 `COPY` 降级链、无 UIDPLUS 时不 `EXPUNGE`、以及写回后的服务器状态。

mod common;

use std::time::Duration;

use common::fake_mail::FakeMail;
use cordis::Context;
use maild::events::MailMessage;
use maild::folders::FolderRole;
use maild::pending::{MessageFlag, ObservedFlags, OpKind, OpState};
use maild::store::Store;
use maild::Accounts;

const NOW: u64 = 1_700_000_000;

fn message(account: &str, folder: &str, uid: &str) -> MailMessage {
    MailMessage {
        account: account.into(),
        uid: uid.into(),
        sender: "sender@example.test".into(),
        subject: format!("subject {uid}"),
        body: "body".into(),
        received_at: NOW,
        folder: folder.into(),
        ..Default::default()
    }
}

fn seeded() -> Store {
    let store = Store::open("").unwrap();
    store
        .upsert_folder("acc", "INBOX", "INBOX", Some(FolderRole::Inbox))
        .unwrap();
    store
        .upsert_folder("acc", "Archive", "Archive", Some(FolderRole::Archive))
        .unwrap();
    for uid in ["1", "2"] {
        store.insert_message(&message("acc", "INBOX", uid)).unwrap();
    }
    store
}

// —— 乐观写与入队 ——

#[test]
fn marking_applies_locally_and_queues_in_one_transaction() {
    let store = seeded();
    let queued = store
        .queue_flag_ops(
            "acc",
            "INBOX",
            &["1".into(), "2".into()],
            MessageFlag::Seen,
            true,
            NOW,
        )
        .unwrap();
    assert_eq!(queued, 2);

    // 本地立刻生效——UI 不等网络。
    let summaries = store
        .list_message_summaries(Some("acc"), Some("INBOX"), 10, 0)
        .unwrap();
    assert!(summaries.iter().all(|summary| summary.seen));

    let ops = store.list_pending_ops(Some("acc"), true, 10).unwrap();
    assert_eq!(ops.len(), 2);
    assert!(ops.iter().all(|op| op.state == OpState::Pending));
    assert!(ops.iter().all(|op| op.kind == OpKind::Flag));
    let payload = ops[0].flag_payload().expect("flag 载荷");
    assert_eq!(payload.flag, MessageFlag::Seen);
    assert!(payload.value);
    assert!(!payload.previous, "记录点击之前的样子，失败时用来还原");

    // 已经是目标状态的消息不重复入队。
    assert_eq!(
        store
            .queue_flag_ops("acc", "INBOX", &["1".into()], MessageFlag::Seen, true, NOW)
            .unwrap(),
        0
    );
    // 库里不存在的 uid 直接跳过，不产生孤儿 op。
    assert_eq!(
        store
            .queue_flag_ops("acc", "INBOX", &["404".into()], MessageFlag::Seen, true, NOW)
            .unwrap(),
        0
    );
}

#[test]
fn moving_hides_the_message_from_the_source_folder_immediately() {
    let store = seeded();
    assert_eq!(
        store
            .queue_move_ops("acc", "INBOX", &["1".into()], "Archive", NOW)
            .unwrap(),
        1
    );
    // 用户看到的就是「它走了」。
    assert_eq!(store.count_messages(Some("acc"), Some("INBOX")).unwrap(), 1);
    assert!(store.get_message("acc", Some("INBOX"), "1").unwrap().is_none());
    // 但行还在——移动失败时它要回来。
    assert_eq!(store.list_pending_ops(Some("acc"), true, 10).unwrap().len(), 1);

    // 重复入队同一条不会产生第二个 op。
    assert_eq!(
        store
            .queue_move_ops("acc", "INBOX", &["1".into()], "Archive", NOW)
            .unwrap(),
        0
    );
}

// —— 状态机 ——

#[test]
fn unknown_is_terminal_for_moves_and_auto_retried_for_flags() {
    let store = seeded();
    store
        .queue_flag_ops("acc", "INBOX", &["1".into()], MessageFlag::Seen, true, NOW)
        .unwrap();
    store
        .queue_move_ops("acc", "INBOX", &["2".into()], "Archive", NOW)
        .unwrap();

    // flag 可重放：结果未知 → 回到 pending 自动重试。
    let flag_op = store.claim_next_op("acc", NOW).unwrap().expect("flag op");
    assert_eq!(flag_op.state, OpState::InFlight);
    assert_eq!(flag_op.attempts, 1);
    let settled = store
        .finish_op(&flag_op, OpState::Unknown, "连接断开", NOW)
        .unwrap();
    assert_eq!(settled, OpState::Pending, "幂等操作重放是安全的");

    // move 不可重放：结果未知是终态，交用户裁决。
    // 回到 pending 的 flag op 会先被取到（id 更小），这本身就说明重试确实排回了队列。
    let mut move_op = None;
    for _ in 0..4 {
        let op = store.claim_next_op("acc", NOW).unwrap().expect("还有待办");
        if op.kind == OpKind::Move {
            move_op = Some(op);
            break;
        }
        store.finish_op(&op, OpState::Done, "", NOW).unwrap();
    }
    let move_op = move_op.expect("move op");
    let settled = store
        .finish_op(&move_op, OpState::Unknown, "连接断开", NOW)
        .unwrap();
    assert_eq!(settled, OpState::Unknown, "重放会产生重复副本，必须交用户裁决");
    // 消息保持隐藏，等用户裁决。
    assert_eq!(store.count_messages(Some("acc"), Some("INBOX")).unwrap(), 1);
}

#[test]
fn flag_retries_are_capped_and_then_ask_the_user() {
    let store = seeded();
    store
        .queue_flag_ops("acc", "INBOX", &["1".into()], MessageFlag::Seen, true, NOW)
        .unwrap();
    let mut last = OpState::Pending;
    for _ in 0..10 {
        let Some(op) = store.claim_next_op("acc", NOW).unwrap() else {
            break;
        };
        last = store.finish_op(&op, OpState::Unknown, "连接断开", NOW).unwrap();
    }
    assert_eq!(last, OpState::Failed, "自动重试有上限，之后交用户裁决");
}

#[test]
fn a_failed_flag_op_restores_what_the_user_saw_before_clicking() {
    let store = seeded();
    store
        .queue_flag_ops("acc", "INBOX", &["1".into()], MessageFlag::Flagged, true, NOW)
        .unwrap();
    let op = store.claim_next_op("acc", NOW).unwrap().unwrap();
    store
        .finish_op(&op, OpState::Failed, "服务器拒绝", NOW)
        .unwrap();
    let message = store.get_message("acc", Some("INBOX"), "1").unwrap().unwrap();
    assert!(!message.flagged, "还原成点击之前的样子");
}

#[test]
fn a_successful_move_removes_the_source_row_and_a_failed_one_brings_it_back() {
    let store = seeded();
    store
        .queue_move_ops("acc", "INBOX", &["1".into(), "2".into()], "Archive", NOW)
        .unwrap();
    let first = store.claim_next_op("acc", NOW).unwrap().unwrap();
    store.finish_op(&first, OpState::Done, "", NOW).unwrap();
    let second = store.claim_next_op("acc", NOW).unwrap().unwrap();
    store
        .finish_op(&second, OpState::Failed, "目标不存在", NOW)
        .unwrap();

    let remaining = store
        .list_message_summaries(Some("acc"), Some("INBOX"), 10, 0)
        .unwrap();
    assert_eq!(remaining.len(), 1, "成功的那条走了，失败的那条回来了");
    assert_eq!(remaining[0].uid, second.uid);
}

#[test]
fn in_flight_ops_are_recovered_after_a_crash() {
    let store = seeded();
    store
        .queue_flag_ops("acc", "INBOX", &["1".into()], MessageFlag::Seen, true, NOW)
        .unwrap();
    store
        .queue_move_ops("acc", "INBOX", &["2".into()], "Archive", NOW)
        .unwrap();
    store.claim_next_op("acc", NOW).unwrap();
    store.claim_next_op("acc", NOW).unwrap();

    // 崩溃时停在 in_flight 与失联同处境。
    assert_eq!(store.recover_in_flight_ops("acc", NOW).unwrap(), 2);
    let ops = store.list_pending_ops(Some("acc"), true, 10).unwrap();
    let flag = ops.iter().find(|op| op.kind == OpKind::Flag).unwrap();
    let moved = ops.iter().find(|op| op.kind == OpKind::Move).unwrap();
    assert_eq!(flag.state, OpState::Pending);
    assert_eq!(moved.state, OpState::Unknown);
}

#[test]
fn user_can_retry_or_discard_an_undecided_op() {
    let store = seeded();
    store
        .queue_move_ops("acc", "INBOX", &["1".into()], "Archive", NOW)
        .unwrap();
    let op = store.claim_next_op("acc", NOW).unwrap().unwrap();
    store.finish_op(&op, OpState::Unknown, "连接断开", NOW).unwrap();

    let retried = store.resolve_op(op.id, true, NOW).unwrap().unwrap();
    assert_eq!(retried.state, OpState::Pending);
    assert_eq!(retried.attempts, 0);

    let claimed = store.claim_next_op("acc", NOW).unwrap().unwrap();
    store
        .finish_op(&claimed, OpState::Unknown, "又断了", NOW)
        .unwrap();
    let discarded = store.resolve_op(op.id, false, NOW).unwrap().unwrap();
    assert_eq!(discarded.state, OpState::Failed);
    assert_eq!(discarded.detail, "用户放弃");
    // 放弃后消息回到列表，由存在性对账收敛真相。
    assert_eq!(store.count_messages(Some("acc"), Some("INBOX")).unwrap(), 2);

    // 还没落定的 op 不接受裁决。
    store
        .queue_flag_ops("acc", "INBOX", &["1".into()], MessageFlag::Seen, true, NOW)
        .unwrap();
    let open = store.list_pending_ops(Some("acc"), true, 10).unwrap()[0].clone();
    let untouched = store.resolve_op(open.id, true, NOW).unwrap().unwrap();
    assert_eq!(untouched.state, OpState::Pending);
}

// —— 对账 ——

fn folder_id(store: &Store, name: &str) -> i64 {
    store.folder_by_name("acc", name).unwrap().unwrap().id
}

#[test]
fn reconcile_takes_the_server_as_truth_but_protects_recent_local_writes() {
    let store = seeded();
    let inbox = folder_id(&store, "INBOX");
    // uid 1：用户刚点了已读，op 还在队列里。
    store
        .queue_flag_ops("acc", "INBOX", &["1".into()], MessageFlag::Seen, true, NOW)
        .unwrap();

    let observed = [
        (1u32, ObservedFlags::default()),
        (
            2u32,
            ObservedFlags {
                seen: true,
                flagged: true,
                ..Default::default()
            },
        ),
    ];
    let outcome = store
        .reconcile_flags(inbox, &observed, true, NOW.saturating_sub(60))
        .unwrap();
    assert_eq!(outcome.updated, 1, "uid 2 以服务器为准");
    assert!(outcome.protected >= 1, "uid 1 有未落定的 op，跳过");

    let one = store.get_message("acc", Some("INBOX"), "1").unwrap().unwrap();
    assert!(one.seen, "用户刚点的已读不被弹回去");
    let two = store.get_message("acc", Some("INBOX"), "2").unwrap().unwrap();
    assert!(two.seen && two.flagged);
}

#[test]
fn recent_local_writes_are_protected_by_the_time_window_alone() {
    let store = seeded();
    let inbox = folder_id(&store, "INBOX");
    store
        .queue_flag_ops("acc", "INBOX", &["1".into()], MessageFlag::Seen, true, NOW)
        .unwrap();
    // op 已经成功落定，队列护栏不再生效——只剩时间窗口挡着。
    let op = store.claim_next_op("acc", NOW).unwrap().unwrap();
    store.finish_op(&op, OpState::Done, "", NOW).unwrap();

    let observed = [(1u32, ObservedFlags::default())];
    store
        .reconcile_flags(inbox, &observed, false, NOW.saturating_sub(60))
        .unwrap();
    assert!(
        store.get_message("acc", Some("INBOX"), "1").unwrap().unwrap().seen,
        "服务器这次 FETCH 还没反映出来，不能覆盖"
    );

    // 窗口之外就以服务器为准。
    store
        .reconcile_flags(inbox, &observed, false, NOW + 1)
        .unwrap();
    assert!(!store.get_message("acc", Some("INBOX"), "1").unwrap().unwrap().seen);
}

#[test]
fn existence_reconcile_only_runs_on_a_full_pass() {
    let store = seeded();
    let inbox = folder_id(&store, "INBOX");
    let observed = [(1u32, ObservedFlags::default())];

    // CHANGEDSINCE 轮次拿不到完整 UID 集合，缺席不代表消失。
    let partial = store
        .reconcile_flags(inbox, &observed, false, NOW + 1)
        .unwrap();
    assert_eq!(partial.removed, 0);
    assert_eq!(store.count_messages(Some("acc"), Some("INBOX")).unwrap(), 2);

    // 全量轮次里缺席 = 被其他客户端 EXPUNGE 了。
    let full = store.reconcile_flags(inbox, &observed, true, NOW + 1).unwrap();
    assert_eq!(full.removed, 1);
    assert!(store.get_message("acc", Some("INBOX"), "2").unwrap().is_none());
}

#[test]
fn condstore_modseq_round_trips() {
    let store = seeded();
    let inbox = folder_id(&store, "INBOX");
    assert_eq!(store.folder_modseq(inbox).unwrap(), None);
    store.set_folder_modseq(inbox, Some(4242)).unwrap();
    assert_eq!(store.folder_modseq(inbox).unwrap(), Some(4242));
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

fn raw_mail(subject: &str) -> Vec<u8> {
    format!(
        "From: sender@test.local\r\nTo: rcpt@test.local\r\n\
         Subject: {subject}\r\nDate: Tue, 1 Mar 2022 10:00:00 +0000\r\n\r\nbody\r\n"
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

async fn online(ctx: &Context) {
    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(
        Duration::from_secs(10),
        accounts.snapshot()[0].2.wait_online(),
    )
    .await
    .expect("10s 内未在线");
}

#[tokio::test(flavor = "current_thread")]
async fn marking_seen_reaches_the_server() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    server.deliver_to("INBOX", "", raw_mail("待读"));

    let ctx = Context::new();
    let mut assembly =
        maild::assemble(&ctx, &assembly_toml(server.imap_addr.port())).expect("装配成功");
    assert!(assembly.errors.is_empty(), "装配错误: {:?}", assembly.errors);
    online(&ctx).await;
    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    wait_until("首封落库", Duration::from_secs(10), || {
        probe.count_messages(Some("e2e"), None).unwrap() == 1
    })
    .await;

    let queued = store
        .queue_flag_ops("e2e", "INBOX", &["1".into()], MessageFlag::Seen, true, NOW)
        .unwrap();
    assert_eq!(queued, 1);
    ctx.get::<Accounts>().unwrap().snapshot()[0].2.wake_writes();

    wait_until("服务器侧置位", Duration::from_secs(10), || {
        server
            .mailbox_uids("INBOX")
            .iter()
            .any(|(uid, flags)| *uid == 1 && flags.contains("\\Seen"))
    })
    .await;
    let probe = store.clone();
    wait_until("op 落定", Duration::from_secs(10), || {
        probe.list_pending_ops(Some("e2e"), true, 10).unwrap().is_empty()
    })
    .await;
    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn move_uses_the_move_command_when_advertised() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    server.set_capabilities("IMAP4rev1 IDLE MOVE UIDPLUS");
    server.add_mailbox("Archive", "\\Archive");
    server.deliver_to("INBOX", "", raw_mail("待归档"));

    let ctx = Context::new();
    let mut assembly =
        maild::assemble(&ctx, &assembly_toml(server.imap_addr.port())).expect("装配成功");
    assert!(assembly.errors.is_empty(), "装配错误: {:?}", assembly.errors);
    online(&ctx).await;
    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    wait_until("首封落库", Duration::from_secs(10), || {
        probe.count_messages(Some("e2e"), Some("INBOX")).unwrap() == 1
    })
    .await;

    store
        .queue_move_ops("e2e", "INBOX", &["1".into()], "Archive", NOW)
        .unwrap();
    ctx.get::<Accounts>().unwrap().snapshot()[0].2.wake_writes();

    wait_until("服务器侧移动完成", Duration::from_secs(10), || {
        server.mailbox_uids("INBOX").is_empty() && server.mailbox_uids("Archive").len() == 1
    })
    .await;
    let commands = server.command_log();
    assert!(
        commands.iter().any(|line| line.contains("UID MOVE")),
        "宣告 MOVE 时应走原子 MOVE: {commands:?}"
    );
    assert!(
        !commands.iter().any(|line| line.contains("UID COPY")),
        "不该退到降级链: {commands:?}"
    );
    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn moving_into_a_missing_folder_fails_instead_of_becoming_undecided() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    server.set_capabilities("IMAP4rev1 IDLE MOVE");
    server.deliver_to("INBOX", "", raw_mail("待归档"));

    let ctx = Context::new();
    let mut assembly =
        maild::assemble(&ctx, &assembly_toml(server.imap_addr.port())).expect("装配成功");
    assert!(assembly.errors.is_empty(), "装配错误: {:?}", assembly.errors);
    online(&ctx).await;
    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    wait_until("首封落库", Duration::from_secs(10), || {
        probe.count_messages(Some("e2e"), Some("INBOX")).unwrap() == 1
    })
    .await;

    // 目标文件夹在服务器上不存在：MOVE 会被明确拒绝。
    store
        .queue_move_ops("e2e", "INBOX", &["1".into()], "不存在的文件夹", NOW)
        .unwrap();
    ctx.get::<Accounts>().unwrap().snapshot()[0].2.wake_writes();

    let probe = store.clone();
    wait_until("op 落定", Duration::from_secs(10), || {
        probe
            .list_pending_ops(Some("e2e"), false, 10)
            .unwrap()
            .iter()
            .any(|op| op.state != OpState::Pending && op.state != OpState::InFlight)
    })
    .await;
    let settled = store.list_pending_ops(Some("e2e"), false, 10).unwrap();
    assert_eq!(
        settled[0].state,
        OpState::Failed,
        "服务器明确拒绝 = 命令没发出，不该挂在「结果未知」上: {:?}",
        settled[0]
    );
    // 消息回到源文件夹。
    assert_eq!(store.count_messages(Some("e2e"), Some("INBOX")).unwrap(), 1);
    assembly.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn move_falls_back_to_copy_and_never_expunges_without_uidplus() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    // 既不宣告 MOVE 也不宣告 UIDPLUS：最保守的服务器。
    server.set_capabilities("IMAP4rev1 IDLE");
    server.add_mailbox("Archive", "\\Archive");
    server.deliver_to("INBOX", "", raw_mail("待归档"));

    let ctx = Context::new();
    let mut assembly =
        maild::assemble(&ctx, &assembly_toml(server.imap_addr.port())).expect("装配成功");
    assert!(assembly.errors.is_empty(), "装配错误: {:?}", assembly.errors);
    online(&ctx).await;
    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    wait_until("首封落库", Duration::from_secs(10), || {
        probe.count_messages(Some("e2e"), Some("INBOX")).unwrap() == 1
    })
    .await;

    store
        .queue_move_ops("e2e", "INBOX", &["1".into()], "Archive", NOW)
        .unwrap();
    ctx.get::<Accounts>().unwrap().snapshot()[0].2.wake_writes();

    wait_until("副本抵达目标", Duration::from_secs(10), || {
        server.mailbox_uids("Archive").len() == 1
    })
    .await;
    let probe = store.clone();
    wait_until("op 落定", Duration::from_secs(10), || {
        probe.list_pending_ops(Some("e2e"), true, 10).unwrap().is_empty()
    })
    .await;

    let commands = server.command_log();
    assert!(
        commands.iter().any(|line| line.contains("UID COPY")),
        "无 MOVE 时降级到 COPY: {commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|line| line.contains("UID STORE") && line.contains("Deleted")),
        "降级链要标记 \\Deleted: {commands:?}"
    );
    assert!(
        !commands.iter().any(|line| line.contains("EXPUNGE")),
        "无 UIDPLUS 时绝不 EXPUNGE——裸 EXPUNGE 会清掉别人标记的邮件: {commands:?}"
    );
    // 源件按 RFC 3501 的删除模型留在原处并带 \Deleted。
    let source = server.mailbox_uids("INBOX");
    assert_eq!(source.len(), 1);
    assert!(source[0].1.contains("\\Deleted"));
    assembly.dispose().await;
}
