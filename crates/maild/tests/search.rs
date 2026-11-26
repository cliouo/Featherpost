//! M4.5-07 本地检索与线程聚合验收（ADR-0029）：中文可搜、查询语法不透传、
//! 结构化过滤、删除不留幻影命中、线程图与顺序无关、回填可中断可续。

use maild::events::MailMessage;
use maild::store::{SearchQuery, Store};

/// 2022-03-01T10:00:00Z
const BASE: u64 = 1_646_128_800;

fn message(uid: &str, subject: &str, body: &str) -> MailMessage {
    MailMessage {
        account: "acc".into(),
        uid: uid.into(),
        sender: "zhang@example.test".into(),
        recipient: "me@example.test".into(),
        subject: subject.into(),
        body: body.into(),
        received_at: BASE,
        date_at: Some(BASE),
        ..Default::default()
    }
}

fn query(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        limit: 50,
        ..Default::default()
    }
}

fn uids(store: &Store, query: &SearchQuery) -> Vec<String> {
    let (messages, total) = store.search_messages(query).unwrap();
    assert_eq!(total, messages.len() as i64, "命中总数与本页一致（未分页时）");
    messages.into_iter().map(|summary| summary.uid).collect()
}

// —— 中文检索：这一片存在的理由 ——

#[test]
fn a_two_character_chinese_word_matches_inside_a_longer_run() {
    let store = Store::open("").unwrap();
    store
        .insert_message(&message("1", "发票已开具", "请查收附件"))
        .unwrap();
    store
        .insert_message(&message("2", "会议纪要", "下周一上午"))
        .unwrap();

    // unicode61 会把「发票已开具」当成一个 token，trigram 又要求查询 ≥3 字符；
    // 逐字切分 + 短语让两字词能命中（ADR-0029 决策 3）。
    assert_eq!(uids(&store, &query("发票")), vec!["1".to_string()]);
    assert_eq!(uids(&store, &query("开具")), vec!["1".to_string()]);
    assert_eq!(uids(&store, &query("纪要")), vec!["2".to_string()]);
    // 正文同样进索引。
    assert_eq!(uids(&store, &query("附件")), vec!["1".to_string()]);
}

#[test]
fn a_phrase_requires_adjacency_so_it_does_not_match_scattered_characters() {
    let store = Store::open("").unwrap();
    store
        .insert_message(&message("1", "发货通知", "票据另行寄出"))
        .unwrap();
    // 「发」和「票」都出现了，但不相邻——短语匹配要求位置相邻，等价于原文子串。
    assert!(uids(&store, &query("发票")).is_empty());
    assert_eq!(uids(&store, &query("发货")), vec!["1".to_string()]);
}

#[test]
fn latin_words_and_addresses_still_match() {
    let store = Store::open("").unwrap();
    store
        .insert_message(&message("1", "Invoice attached", "See the PDF"))
        .unwrap();
    assert_eq!(uids(&store, &query("invoice")), vec!["1".to_string()]);
    assert_eq!(uids(&store, &query("INVOICE")), vec!["1".to_string()], "大小写不敏感");
    // `@` 与 `.` 是 unicode61 的分隔符；完整地址作为短语下发，一样命中。
    assert_eq!(
        uids(&store, &query("zhang@example.test")),
        vec!["1".to_string()]
    );
    assert!(uids(&store, &query("invoice missing")).is_empty(), "多个词是 AND");
}

#[test]
fn fts5_syntax_in_the_query_neither_errors_nor_leaks_into_the_expression() {
    let store = Store::open("").unwrap();
    store
        .insert_message(&message("1", "quarterly report", "numbers inside"))
        .unwrap();
    // 这些串全是 FTS5 语法元素。透传会让搜索直接报错或跨列检索；
    // 内核构造表达式后它们只是普通文本，搜不到就是搜不到（ADR-0029 决策 4）。
    for text in [
        "\"",
        "report OR nonexistent",
        "subject:report",
        "report*",
        "NEAR(report numbers)",
        "-report",
        "(report",
    ] {
        let result = store.search_messages(&query(text));
        assert!(result.is_ok(), "查询 {text:?} 不得让检索失败: {result:?}");
    }
    // `OR` 被当成一个普通词，因此整体是 AND 三个词，命中为空。
    assert!(uids(&store, &query("report OR nonexistent")).is_empty());
    assert_eq!(uids(&store, &query("report")), vec!["1".to_string()]);
}

// —— 结构化过滤 ——

#[test]
fn structured_filters_narrow_the_result_without_entering_the_match_expression() {
    let store = Store::open("").unwrap();
    for (uid, sender, folder, sort_at, attachments) in [
        ("1", "zhang@example.test", "INBOX", BASE, true),
        ("2", "li@other.test", "INBOX", BASE + 86_400, false),
        ("3", "zhang@example.test", "Archive", BASE + 2 * 86_400, false),
    ] {
        store
            .insert_message(&MailMessage {
                uid: uid.into(),
                sender: sender.into(),
                folder: folder.into(),
                // 观测时间同步前移：Date 领先观测时间超过一天会被判为不可信
                // 而退回 received_at（ADR-0025 决策 3），那样这里就测不到时间过滤。
                received_at: sort_at,
                date_at: Some(sort_at),
                has_attachments: attachments,
                ..message(uid, "报告", "正文")
            })
            .unwrap();
    }

    let sender_only = SearchQuery {
        sender: Some("ZHANG".into()),
        ..query("报告")
    };
    assert_eq!(uids(&store, &sender_only), vec!["3".to_string(), "1".to_string()]);

    let folder_only = SearchQuery {
        folder: Some("Archive".into()),
        ..query("报告")
    };
    assert_eq!(uids(&store, &folder_only), vec!["3".to_string()]);

    let window = SearchQuery {
        since: Some(BASE + 86_400),
        until: Some(BASE + 86_400),
        ..query("报告")
    };
    assert_eq!(uids(&store, &window), vec!["2".to_string()]);

    let attachments = SearchQuery {
        has_attachments: Some(true),
        ..query("报告")
    };
    assert_eq!(uids(&store, &attachments), vec!["1".to_string()]);

    // 没有查询词时退化为纯过滤，而不是报错或返回空。
    let filter_only = SearchQuery {
        has_attachments: Some(false),
        ..query("   ")
    };
    assert_eq!(uids(&store, &filter_only), vec!["3".to_string(), "2".to_string()]);
}

#[test]
fn extended_structured_filters_match_recipient_cc_subject_flags_literals_and_legacy_rows() {
    let store = Store::open("").unwrap();

    // 1. To 命中、含 % 和 _ 字面量、未读且星标。
    store
        .insert_message(&MailMessage {
            uid: "1".into(),
            sender: "boss_100%@example.test".into(),
            recipient: "Alice_Team <alice_50%@example.test>".into(),
            cc: String::new(),
            subject: "Q4_Budget 100% Final".into(),
            received_at: BASE,
            date_at: Some(BASE),
            seen: false,
            flagged: true,
            ..message("1", "Q4_Budget 100% Final", "body one")
        })
        .unwrap();

    // 2. 仅 Cc 命中、已读且未星标、不含 % / _ 字面量。
    store
        .insert_message(&MailMessage {
            uid: "2".into(),
            sender: "ops@example.test".into(),
            recipient: "primary@example.test".into(),
            cc: "Bob <bob@example.test>, Carol <carol@example.test>".into(),
            subject: "Q4XBudget 1000 Final".into(),
            received_at: BASE + 100,
            date_at: Some(BASE + 100),
            seen: true,
            flagged: false,
            ..message("2", "Q4XBudget 1000 Final", "body two")
        })
        .unwrap();

    // 3. 兼容旧库空字段行（recipient / cc / subject 均为空串）、已读且星标。
    store
        .insert_message(&MailMessage {
            uid: "3".into(),
            sender: "legacy@example.test".into(),
            recipient: String::new(),
            cc: String::new(),
            subject: String::new(),
            received_at: BASE + 200,
            date_at: Some(BASE + 200),
            seen: true,
            flagged: true,
            ..message("3", "", "legacy body")
        })
        .unwrap();

    // 4. 仅 Cc 命中且同时满足未读 + 星标，用于验证括号优先级：
    //    `((recipient) OR (cc)) AND seen = 0 AND flagged = 1` 不得因缺括号泄漏 OR。
    store
        .insert_message(&MailMessage {
            uid: "4".into(),
            sender: "ops@example.test".into(),
            recipient: "nobody@example.test".into(),
            cc: "Bob <bob@example.test>".into(),
            subject: "季度复盘".into(),
            received_at: BASE + 300,
            date_at: Some(BASE + 300),
            seen: false,
            flagged: true,
            ..message("4", "季度复盘", "body four")
        })
        .unwrap();

    // `to` 匹配 recipient（大小写不敏感）。
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                recipient: Some("ALICE_50%".into()),
                ..query("")
            }
        ),
        vec!["1".to_string()]
    );

    // `to` 仅命中 Cc（不漏抄送，且空旧行不会被误命中）。
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                recipient: Some("BOB@EXAMPLE.TEST".into()),
                ..query("")
            }
        ),
        vec!["4".to_string(), "2".to_string()]
    );

    // `to` 与 `seen` / `flagged` 组合 AND：验证 `recipient OR cc` 被正确括号包裹。
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                recipient: Some("bob@example.test".into()),
                seen: Some(false),
                flagged: Some(true),
                ..query("")
            }
        ),
        vec!["4".to_string()]
    );
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                recipient: Some("bob@example.test".into()),
                seen: Some(true),
                flagged: Some(false),
                ..query("")
            }
        ),
        vec!["2".to_string()]
    );

    // `subject` 字面子串：`_` 与 `%` 按字面匹配，不当作 SQL LIKE 通配符。
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                subject: Some("q4_budget 100%".into()),
                ..query("")
            }
        ),
        vec!["1".to_string()],
        "uid 2 的 Q4XBudget 1000 不应被 _ 或 % 通配命中"
    );
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                subject: Some("复盘".into()),
                ..query("")
            }
        ),
        vec!["4".to_string()]
    );

    // `seen` 与 `flagged` 独立过滤（空 query 跳过 FTS）。
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                seen: Some(false),
                ..query("")
            }
        ),
        vec!["4".to_string(), "1".to_string()]
    );
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                seen: Some(true),
                ..query("")
            }
        ),
        vec!["3".to_string(), "2".to_string()]
    );
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                flagged: Some(true),
                ..query("")
            }
        ),
        vec!["4".to_string(), "3".to_string(), "1".to_string()]
    );
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                flagged: Some(false),
                ..query("")
            }
        ),
        vec!["2".to_string()]
    );

    // 分页下 COUNT 与分页 SELECT 共用同一谓词。
    let (page, total) = store
        .search_messages(&SearchQuery {
            flagged: Some(true),
            limit: 2,
            offset: 1,
            ..query("")
        })
        .unwrap();
    assert_eq!(total, 3);
    assert_eq!(
        page.into_iter().map(|s| s.uid).collect::<Vec<_>>(),
        vec!["3".to_string(), "1".to_string()]
    );

    // 空旧行不匹配任何非空 `to` / `subject` 筛选；纯空白筛选不影响空旧行。
    assert!(uids(
        &store,
        &SearchQuery {
            recipient: Some("%".into()),
            ..query("")
        }
    )
    .iter()
    .all(|uid| uid != "3"));
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                recipient: Some("   ".into()),
                subject: Some("   ".into()),
                ..query("")
            }
        ),
        vec!["4".to_string(), "3".to_string(), "2".to_string(), "1".to_string()]
    );

    // 乐观删除/移动（pending_removal = 1）在纯结构化过滤下同样不可见。
    store
        .queue_move_ops("acc", "INBOX", &["4".into()], "Trash", BASE + 400)
        .unwrap();
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                recipient: Some("bob@example.test".into()),
                ..query("")
            }
        ),
        vec!["2".to_string()]
    );
    assert_eq!(
        uids(
            &store,
            &SearchQuery {
                seen: Some(false),
                flagged: Some(true),
                ..query("")
            }
        ),
        vec!["1".to_string()]
    );
}

#[test]
fn results_are_ordered_by_the_same_key_as_the_list() {
    let store = Store::open("").unwrap();
    for (uid, date_at) in [("old", BASE), ("new", BASE + 86_400), ("mid", BASE + 3_600)] {
        store
            .insert_message(&MailMessage {
                received_at: date_at,
                date_at: Some(date_at),
                ..message(uid, "报告", "x")
            })
            .unwrap();
    }
    assert_eq!(
        uids(&store, &query("报告")),
        vec!["new".to_string(), "mid".to_string(), "old".to_string()],
        "按 sort_at 倒序，与 listMessageSummaries 一致（ADR-0029 决策 6）"
    );
}

#[test]
fn optimistically_removed_messages_are_invisible_to_search() {
    let store = Store::open("").unwrap();
    store.insert_message(&message("1", "发票", "x")).unwrap();
    assert_eq!(uids(&store, &query("发票")).len(), 1);
    // 乐观移动/删除期间该行对列表不可见（ADR-0027 决策 5），检索必须同口径。
    store
        .queue_move_ops("acc", "INBOX", &["1".into()], "Trash", BASE)
        .unwrap();
    assert!(uids(&store, &query("发票")).is_empty());
}

// —— 删除方向由触发器兜底 ——

#[test]
fn deleting_a_message_removes_it_from_the_index_without_store_side_help() {
    let store = Store::open("").unwrap();
    store.insert_message(&message("1", "发票", "x")).unwrap();
    store.insert_message(&message("2", "发票", "y")).unwrap();
    assert_eq!(uids(&store, &query("发票")).len(), 2);

    let folder = store.folder_by_name("acc", "INBOX").unwrap().unwrap();
    // rebuild_folder 只删 messages 行；索引与线程边靠 SQL 触发器一起消失。
    // 漏掉这个方向，搜索会命中一封已经不存在的邮件（ADR-0029 决策 10）。
    store.rebuild_folder(folder.id, 999).unwrap();
    assert!(uids(&store, &query("发票")).is_empty(), "不得留下幻影命中");

    // 索引确实空了，而不是被 pending_removal 之类的过滤挡住。
    let status = store.search_index_status().unwrap();
    assert!(status.done);
}

// —— 线程聚合 ——

fn threaded(uid: &str, message_id: &str, in_reply_to: &str, references: &str) -> MailMessage {
    MailMessage {
        message_id: message_id.into(),
        in_reply_to: in_reply_to.into(),
        references: references.into(),
        ..message(uid, "会议安排", "正文")
    }
}

#[test]
fn a_reply_joins_the_thread_of_the_message_it_answers() {
    let store = Store::open("").unwrap();
    store.insert_message(&threaded("1", "a@x", "", "")).unwrap();
    store.insert_message(&threaded("2", "b@x", "a@x", "a@x")).unwrap();
    store
        .insert_message(&threaded("3", "c@x", "b@x", "a@x b@x"))
        .unwrap();

    let thread = store.thread_of("acc", Some("INBOX"), "1").unwrap().unwrap();
    let members: Vec<String> = store
        .thread_summaries("acc", thread)
        .unwrap()
        .into_iter()
        .map(|summary| summary.uid)
        .collect();
    assert_eq!(members, vec!["1".to_string(), "2".to_string(), "3".to_string()]);
    for uid in ["2", "3"] {
        assert_eq!(store.thread_of("acc", Some("INBOX"), uid).unwrap(), Some(thread));
    }
}

#[test]
fn out_of_order_arrival_converges_to_the_same_thread() {
    let store = Store::open("").unwrap();
    // 回复先到：它留下一条指向尚未见过的父信的边。
    store.insert_message(&threaded("2", "b@x", "a@x", "a@x")).unwrap();
    let early = store.thread_of("acc", Some("INBOX"), "2").unwrap().unwrap();
    // 父信后到，用自己的 Message-ID 查到那条边并入（ADR-0029 决策 7）。
    store.insert_message(&threaded("1", "a@x", "", "")).unwrap();
    assert_eq!(store.thread_of("acc", Some("INBOX"), "1").unwrap(), Some(early));
    assert_eq!(store.thread_summaries("acc", early).unwrap().len(), 2);
}

#[test]
fn a_message_referencing_two_threads_merges_them() {
    let store = Store::open("").unwrap();
    store.insert_message(&threaded("1", "a@x", "", "")).unwrap();
    store.insert_message(&threaded("2", "b@x", "", "")).unwrap();
    let first = store.thread_of("acc", Some("INBOX"), "1").unwrap().unwrap();
    let second = store.thread_of("acc", Some("INBOX"), "2").unwrap().unwrap();
    assert_ne!(first, second, "两条独立线程");

    store
        .insert_message(&threaded("3", "c@x", "b@x", "a@x b@x"))
        .unwrap();
    let merged = store.thread_of("acc", Some("INBOX"), "3").unwrap().unwrap();
    assert_eq!(merged, first.min(second), "并进较小的线程 id");
    assert_eq!(store.thread_summaries("acc", merged).unwrap().len(), 3);
}

#[test]
fn identical_subjects_do_not_merge_unrelated_threads() {
    let store = Store::open("").unwrap();
    // 「你好」这种主题在真实邮箱里天天重复。按主题兜底会把陌生人的邮件并进同一条
    // 线程——错并比漏并严重得多（ADR-0029 决策 8）。
    store.insert_message(&threaded("1", "a@x", "", "")).unwrap();
    store.insert_message(&threaded("2", "b@y", "", "")).unwrap();
    let first = store.thread_of("acc", Some("INBOX"), "1").unwrap().unwrap();
    let second = store.thread_of("acc", Some("INBOX"), "2").unwrap().unwrap();
    assert_ne!(first, second);
    assert_eq!(store.thread_summaries("acc", first).unwrap().len(), 1);
}

#[test]
fn a_thread_spans_folders_because_a_conversation_does() {
    let store = Store::open("").unwrap();
    store.insert_message(&threaded("1", "a@x", "", "")).unwrap();
    store
        .insert_message(&MailMessage {
            folder: "Sent".into(),
            ..threaded("9", "b@x", "a@x", "a@x")
        })
        .unwrap();
    let thread = store.thread_of("acc", Some("INBOX"), "1").unwrap().unwrap();
    let folders: Vec<String> = store
        .thread_summaries("acc", thread)
        .unwrap()
        .into_iter()
        .map(|summary| summary.folder)
        .collect();
    assert_eq!(folders, vec!["INBOX".to_string(), "Sent".to_string()]);
}

#[test]
fn threads_do_not_cross_accounts() {
    let store = Store::open("").unwrap();
    store.insert_message(&threaded("1", "a@x", "", "")).unwrap();
    store
        .insert_message(&MailMessage {
            account: "other".into(),
            ..threaded("2", "b@x", "a@x", "a@x")
        })
        .unwrap();
    let first = store.thread_of("acc", Some("INBOX"), "1").unwrap().unwrap();
    assert_eq!(store.thread_summaries("acc", first).unwrap().len(), 1);
    assert_eq!(store.thread_summaries("other", first).unwrap().len(), 0);
}

// —— 回填可中断可续 ——

#[test]
fn backfill_resumes_from_its_cursor_and_never_repeats_or_skips() {
    let store = Store::open("").unwrap();
    for index in 0..10 {
        store
            .insert_message(&message(&index.to_string(), "报告", "正文"))
            .unwrap();
    }
    assert_eq!(uids(&store, &query("报告")).len(), 10);

    // 模拟「升级后索引为空」：丢掉索引并把游标重置到 MAX(id)。
    store.rebuild_search_index().unwrap();
    assert!(uids(&store, &query("报告")).is_empty());
    let status = store.search_index_status().unwrap();
    assert_eq!((status.pending, status.done), (10, false));

    // 一次只推进 3 条，中途「崩溃」——下一次从游标续跑。
    let first = store.backfill_search_index(3).unwrap();
    assert_eq!((first.pending, first.done), (7, false));
    assert_eq!(uids(&store, &query("报告")).len(), 3, "最近的先可搜");

    let second = store.backfill_search_index(3).unwrap();
    assert_eq!((second.pending, second.done), (4, false));
    assert_eq!(uids(&store, &query("报告")).len(), 6);

    while !store.backfill_search_index(3).unwrap().done {}
    assert_eq!(uids(&store, &query("报告")).len(), 10, "不重复也不遗漏");

    // 完成之后再调用是空转，不会把已索引的行再写一遍。
    let after = store.backfill_search_index(3).unwrap();
    assert_eq!((after.pending, after.done), (0, true));
    assert_eq!(uids(&store, &query("报告")).len(), 10);
}

#[test]
fn backfill_and_live_delivery_converge_on_the_same_thread() {
    let store = Store::open("").unwrap();
    store.insert_message(&threaded("1", "a@x", "", "")).unwrap();
    store.rebuild_search_index().unwrap();

    // 回填还没跑到旧信，新回复先落库：它自己开一条线程。
    store.insert_message(&threaded("2", "b@x", "a@x", "a@x")).unwrap();
    let live = store.thread_of("acc", Some("INBOX"), "2").unwrap().unwrap();
    assert_ne!(live, 0);

    // 回填补上旧信，两边合并——顺序无关（ADR-0029 决策 7）。
    while !store.backfill_search_index(1).unwrap().done {}
    let old = store.thread_of("acc", Some("INBOX"), "1").unwrap().unwrap();
    assert_eq!(store.thread_summaries("acc", old).unwrap().len(), 2);
}

#[test]
fn a_fresh_database_has_nothing_to_backfill() {
    let store = Store::open("").unwrap();
    let status = store.search_index_status().unwrap();
    assert_eq!((status.pending, status.done), (0, true));
    // 新落库的消息由插入路径直接索引，不经回填。
    store.insert_message(&message("1", "发票", "x")).unwrap();
    assert_eq!(uids(&store, &query("发票")).len(), 1);
    assert!(store.search_index_status().unwrap().done);
}

// —— 服务器侧检索（ADR-0029 决策 11）——

mod common;

use common::fake_mail::FakeMail;
use cordis::Context;
use maild::Accounts;
use std::time::Duration;

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
async fn server_search_runs_on_the_receive_connection_and_returns_summaries() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    server.add_mailbox("Archive", "\\Archive");
    // 归档里的历史邮件；正常同步也会拉到，这里只关心 SEARCH 这条路走通。
    server.deliver_to("Archive", "\\Seen", raw_mail("年度报告", "第四季度数据"));
    server.deliver_to("Archive", "\\Seen", raw_mail("午餐安排", "楼下食堂"));

    let ctx = Context::new();
    let assembly =
        maild::assemble(&ctx, &assembly_toml(server.imap_addr.port())).expect("装配成功");
    assert!(assembly.errors.is_empty(), "装配错误: {:?}", assembly.errors);

    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    let receive = accounts.snapshot().swap_remove(0).2;
    tokio::time::timeout(Duration::from_secs(10), receive.wait_online())
        .await
        .expect("10s 内未在线");

    let hits = tokio::time::timeout(
        Duration::from_secs(10),
        receive.server_search("Archive", "年度报告", None, None),
    )
    .await
    .expect("服务器检索未在 10s 内返回")
    .expect("服务器检索成功");
    assert_eq!(hits.len(), 1, "命中: {hits:?}");
    assert_eq!(hits[0].subject, "年度报告");
    assert_eq!(hits[0].folder, "Archive", "结果标注来自哪个信箱");
    assert!(hits[0].seen, "标记位如实带回");

    // 非 ASCII 查询必须声明字符集，否则服务器无从解释这串字节。
    let commands = server.command_log();
    let search = commands
        .iter()
        .find(|line| line.to_ascii_uppercase().contains("UID SEARCH"))
        .expect("发出了 UID SEARCH");
    assert!(search.contains("CHARSET UTF-8"), "实际命令: {search}");

    let empty = receive
        .server_search("Archive", "不存在的词", None, None)
        .await
        .expect("空结果不是错误");
    assert!(empty.is_empty());

    // 检索走的是收信那条连接，不另开——ADR-0026 把连接预算上限定成了 1。
    assert_eq!(server.imap_connections(), 1, "服务器检索不得新建连接");

    drop(assembly);
}

#[tokio::test(flavor = "current_thread")]
async fn internal_date_anchors_decade_old_mail_clamps_spoofed_future_and_filters_server_search() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    server.add_mailbox("Archive", "\\Archive");

    // 1. >10 年前的历史邮件（2010-05-15 08:30:00Z = 1_273_912_200）：
    //    若 `received_at` 取本机 `now` 会因超出 10 年容忍窗口回退到 `now`，有了 INTERNALDATE 后保持 2010 年。
    let mail_2010 = "From: Archive <old@test.local>\r\nTo: me@test.local\r\n\
Subject: 2010 归档合同\r\nDate: Sat, 15 May 2010 08:30:00 +0000\r\n\r\ncommon-token 2010\r\n"
        .as_bytes()
        .to_vec();
    server.deliver_to_with_internal_date(
        "INBOX",
        "\\Seen",
        Some("15-May-2010 08:30:00 +0000"),
        mail_2010.clone(),
    );
    server.deliver_to_with_internal_date(
        "Archive",
        "\\Seen",
        Some("15-May-2010 08:30:00 +0000"),
        mail_2010,
    );

    // 2. 缺失 Date 头的历史邮件（2011-06-01 00:00:00Z = 1_306_886_400）：靠 INTERNALDATE 锚定。
    let dateless_2011 = "From: Archive <nodate@test.local>\r\nTo: me@test.local\r\n\
Subject: 无日期老信\r\n\r\ncommon-token 2011\r\n"
        .as_bytes()
        .to_vec();
    server.deliver_to_with_internal_date(
        "Archive",
        "\\Seen",
        Some("01-Jun-2011 00:00:00 +0000"),
        dateless_2011,
    );

    // 3. 伪造远未来 Date 且服务器省略 INTERNALDATE：server_search 中必须钳制为 0，绝不排到顶部。
    let spoofed_future = "From: Spam <spam@test.local>\r\nTo: me@test.local\r\n\
Subject: 伪造 2100\r\nDate: Fri, 01 Jan 2100 00:00:00 +0000\r\n\r\ncommon-token future\r\n"
        .as_bytes()
        .to_vec();
    server.deliver_to_with_internal_date("Archive", "\\Seen", None, spoofed_future);

    let ctx = Context::new();
    let mut assembly =
        maild::assemble(&ctx, &assembly_toml(server.imap_addr.port())).expect("装配成功");
    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    let receive = accounts.snapshot().swap_remove(0).2;
    tokio::time::timeout(Duration::from_secs(10), receive.wait_online())
        .await
        .expect("10s 内未在线");

    let store = ctx.get::<Store>().expect("store 服务");
    let probe = store.clone();
    let until_stored = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < until_stored {
        if probe.get_message("e2e", Some("INBOX"), "1").unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let synced_old = store
        .get_message("e2e", Some("INBOX"), "1")
        .unwrap()
        .expect("INBOX 2010 老信已落库");
    assert_eq!(synced_old.received_at, 1_273_912_200);
    assert_eq!(synced_old.sort_at, 1_273_912_200, ">10 年老信不应被排成今日");

    let all_hits = receive
        .server_search("Archive", "common-token", None, None)
        .await
        .expect("server_search 成功");
    assert_eq!(all_hits.len(), 3);
    // 按 sort_at 降序：2011 (1_306_886_400) -> 2010 (1_273_912_200) -> 伪造未来无 INTERNALDATE (0)
    assert_eq!(
        all_hits.iter().map(|s| s.sort_at).collect::<Vec<_>>(),
        vec![1_306_886_400, 1_273_912_200, 0]
    );

    // since / until 边界过滤（含通过 API scope=server）。
    let only_2010 = receive
        .server_search(
            "Archive",
            "common-token",
            Some(1_270_000_000),
            Some(1_280_000_000),
        )
        .await
        .unwrap();
    assert_eq!(only_2010.len(), 1);
    assert_eq!(only_2010[0].subject, "2010 归档合同");

    // 按需单封拉取缺失 Date 的 2011 老信，同样保留 INTERNALDATE。
    let fetched_2011 = receive
        .fetch_message_by_uid("Archive", 2)
        .await
        .unwrap()
        .expect("Archive UID 2");
    assert_eq!(fetched_2011.received_at, 1_306_886_400);
    assert_eq!(fetched_2011.sort_at, 1_306_886_400);

    assembly.dispose().await;
}

/// 回填任务在 `spawn_blocking` 里持有的是真 `Arc<Store>`。卸载只发停止信号加 `abort()`
/// 的话，那一批会继续跑完并一直扣着数据库锁——daemon 卸载完成后重新打开库就会失败。
/// 这条钉死的是「dispose 返回时没有任何游离引用」（ADR-0029 决策 9 / ADR-0026 决策 11）。
#[tokio::test(flavor = "current_thread")]
async fn disposing_the_store_plugin_leaves_no_straggler_holding_the_database_lock() {
    use maild::store::{StoreConfig, StorePlugin};
    use std::sync::Arc;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mail.sqlite3");
    let path_str = path.to_str().unwrap().to_string();

    // 每批 200 条，正文足够大到一批要跑上一会儿——卸载信号一定落在批次中间。
    let body = "发票已开具，请查收附件并及时确认金额。".repeat(200);
    {
        let store = Store::open(&path_str).unwrap();
        for index in 0..600 {
            store
                .insert_message(&MailMessage {
                    body: body.clone(),
                    ..message(&index.to_string(), "报告", "")
                })
                .unwrap();
        }
        // 丢掉索引，让挂载后的后台任务确实有活干。
        store.rebuild_search_index().unwrap();
        assert!(!store.search_index_status().unwrap().done);
    }

    for round in 0..3 {
        let ctx = cordis::Context::new();
        ctx.provide::<StoreConfig>(Arc::new(StoreConfig {
            db_path: path_str.clone(),
        }));
        let handle = ctx.plugin(Arc::new(StorePlugin));
        // 让回填进到批次内部再拔插头。
        tokio::time::sleep(Duration::from_millis(5)).await;
        handle.dispose().await;

        // 卸载返回的那一刻，库必须是可以立刻再打开的。
        let reopened = Store::open(&path_str)
            .unwrap_or_else(|error| panic!("第 {round} 轮卸载后打不开库: {error}"));
        drop(reopened);
    }
}
