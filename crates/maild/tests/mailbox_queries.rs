use maild::{
    events::MailMessage,
    folders::FolderRole,
    pending::{MessageFlag, OpState},
    store::{Store, SummaryFilter},
};
fn message(account: &str, folder: &str, uid: &str, at: u64) -> MailMessage {
    MailMessage {
        account: account.into(),
        folder: folder.into(),
        uid: uid.into(),
        subject: format!("测试 {uid}"),
        body: "本地正文".into(),
        received_at: at,
        date_at: Some(at),
        ..Default::default()
    }
}
#[test]
fn unified_inbox_uses_roles_and_filters_before_pagination() {
    let store = Store::open("").unwrap();
    for (account, folder, role) in [
        ("a", "Inbox-custom", FolderRole::Inbox),
        ("b", "收件箱", FolderRole::Inbox),
        ("a", "Sent", FolderRole::Sent),
    ] {
        store
            .upsert_folder(account, folder, folder, Some(role))
            .unwrap();
    }
    for m in [
        message("a", "Inbox-custom", "1", 100),
        message("b", "收件箱", "1", 100),
        message("a", "Sent", "1", 200),
    ] {
        store.insert_message(&m).unwrap();
    }
    let query = SummaryFilter {
        role: Some("inbox".into()),
        ..Default::default()
    };
    let (first, total) = store.summary_page(&query, 1, 0).unwrap();
    let (second, total2) = store.summary_page(&query, 1, 1).unwrap();
    assert_eq!((total, total2), (2, 2));
    assert_eq!(first[0].account, "b");
    assert_eq!(second[0].account, "a");
    assert!(store.summary_page(&query, 1, 2).unwrap().0.is_empty());
    store
        .queue_flag_ops(
            "a",
            "Inbox-custom",
            &["1".into()],
            MessageFlag::Seen,
            true,
            300,
        )
        .unwrap();
    let (_, total) = store
        .summary_page(
            &SummaryFilter {
                seen: Some(false),
                ..query
            },
            50,
            0,
        )
        .unwrap();
    assert_eq!(total, 1);
    let (_, total) = store
        .summary_page(
            &SummaryFilter {
                account: Some("a' OR 1=1 --".into()),
                ..Default::default()
            },
            50,
            0,
        )
        .unwrap();
    assert_eq!(total, 0);
}
#[test]
fn thread_page_is_chronological_and_account_scoped_across_folders() {
    let store = Store::open("").unwrap();
    for (account, folder, uid, at, id, parent) in [
        ("a", "INBOX", "1", 100, "root@x", ""),
        ("a", "Sent", "1", 200, "reply@x", "root@x"),
        ("b", "INBOX", "1", 150, "other@x", "root@x"),
    ] {
        let mut m = message(account, folder, uid, at);
        m.message_id = id.into();
        m.in_reply_to = parent.into();
        store.insert_message(&m).unwrap();
    }
    let id = store.thread_of("a", Some("INBOX"), "1").unwrap().unwrap();
    let (first, total) = store.thread_page("a", id, 1, 0).unwrap();
    let (second, total2) = store.thread_page("a", id, 1, 1).unwrap();
    assert_eq!((total, total2), (2, 2));
    assert_eq!(first[0].folder, "INBOX");
    assert_eq!(second[0].folder, "Sent");
    assert!(store.thread_page("b", id, 10, 0).unwrap().0.is_empty());
}
#[test]
fn activity_summary_skips_large_append_bytes_and_pages_past_settled_failures() {
    let store = Store::open("").unwrap();
    store
        .upsert_folder("a", "Sent", "Sent", Some(FolderRole::Sent))
        .unwrap();
    let raw = format!(
        "Subject: private marker\r\n\r\n{}",
        "x".repeat(2 * 1024 * 1024)
    );
    store.queue_append_op("a", "Sent", &raw, 100).unwrap();
    let op = store.claim_next_op("a", 101).unwrap().unwrap();
    store
        .finish_op(&op, OpState::Unknown, "连接中断", 102)
        .unwrap();
    for i in 0..3 {
        let uid = i.to_string();
        store
            .insert_message(&message("a", "INBOX", &uid, 100))
            .unwrap();
        store
            .queue_flag_ops("a", "INBOX", &[uid], MessageFlag::Seen, true, 103)
            .unwrap();
    }
    let first = store
        .pending_op_page(Some("a"), false, 2, None, true)
        .unwrap();
    let next = store
        .pending_op_page(Some("a"), false, 2, Some(first[1].id), true)
        .unwrap();
    assert_eq!(first.len() + next.len(), 4);
    assert!(next.last().unwrap().payload.is_empty());
    assert_eq!(next.last().unwrap().state, OpState::Unknown);
    assert!(first.iter().all(|o| o.id > next[0].id));
    let raw_read = store
        .pending_op_page(Some("a"), false, 10, None, false)
        .unwrap();
    assert!(raw_read.last().unwrap().payload.contains("private marker"));
    store.resolve_op(op.id, false, 104).unwrap();
    let settled = store
        .pending_op_page(Some("a"), false, 10, None, true)
        .unwrap();
    assert_eq!(settled.last().unwrap().state, OpState::Failed);
    assert!(store
        .pending_op_page(Some("a"), true, 10, None, true)
        .unwrap()
        .iter()
        .all(|o| o.id != op.id));
}

#[test]
fn write_receipts_distinguish_noop_missing_and_newly_queued_in_one_transaction() {
    let store = Store::open("").unwrap();
    let mut read = message("a", "INBOX", "1", 100);
    read.seen = true;
    store.insert_message(&read).unwrap();
    store
        .insert_message(&message("a", "INBOX", "2", 101))
        .unwrap();
    let r = store
        .queue_flag_receipt(
            "a",
            "INBOX",
            &["1".into(), "2".into(), "3".into()],
            MessageFlag::Seen,
            true,
            200,
        )
        .unwrap();
    assert_eq!(r.queued_uids, vec!["2"]);
    assert_eq!(r.unchanged_uids, vec!["1"]);
    assert_eq!(r.missing_uids, vec!["3"]);
    let again = store
        .queue_flag_receipt(
            "a",
            "INBOX",
            &["1".into(), "2".into()],
            MessageFlag::Seen,
            true,
            201,
        )
        .unwrap();
    assert!(again.queued_uids.is_empty());
    assert_eq!(again.unchanged_uids.len(), 2);
    assert_eq!(
        store.list_pending_ops(Some("a"), false, 10).unwrap().len(),
        1
    );
}
