//! M4.5-06 发信补全、回复/转发与 Sent 归档验收（ADR-0028）。
//!
//! 组装出的报文一律用 `mailparse` 解回来断言——「我们写出去的东西别人能解析」
//! 是这一片唯一有意义的验收标准。

mod common;

use std::time::Duration;

use common::fake_mail::FakeMail;
use cordis::Context;
use maild::compose::{self, OutgoingAttachment, OutgoingMessage};
use maild::events::{MailAttachment, MailMessage};
use maild::folders::FolderRole;
use maild::pending::{OpKind, OpState};
use maild::store::Store;
use maild::Accounts;
use mailparse::{MailHeaderMap, ParsedMail};

const NOW: u64 = 1_646_128_800; // 2022-03-01T10:00:00Z

fn base(subject: &str, body: &str) -> OutgoingMessage {
    OutgoingMessage {
        from: "me@example.test".into(),
        to: vec!["alice@example.test".into()],
        subject: subject.into(),
        body: body.into(),
        message_id: compose::new_message_id("me@example.test", NOW),
        date_unix: NOW,
        ..Default::default()
    }
}

fn parsed(raw: &str) -> ParsedMail<'_> {
    mailparse::parse_mail(raw.as_bytes()).expect("组装出的报文必须可解析")
}

fn header(mail: &ParsedMail, name: &str) -> String {
    mail.headers.get_first_value(name).unwrap_or_default()
}

/// 深度优先收集所有叶子 part 的 (mimetype, 解码后字节)。
fn leaves<'a>(mail: &'a ParsedMail<'a>, out: &mut Vec<(String, Vec<u8>)>) {
    if mail.subparts.is_empty() {
        out.push((
            mail.ctype.mimetype.clone(),
            mail.get_body_raw().unwrap_or_default(),
        ));
        return;
    }
    for sub in &mail.subparts {
        leaves(sub, out);
    }
}

// —— 头部与编码 ——

#[test]
fn a_plain_message_round_trips_through_a_real_parser() {
    let raw = compose::assemble(&base("Weekly sync", "hello there"));
    let mail = parsed(&raw);
    assert_eq!(header(&mail, "From"), "me@example.test");
    assert_eq!(header(&mail, "To"), "alice@example.test");
    assert_eq!(header(&mail, "Subject"), "Weekly sync");
    assert_eq!(header(&mail, "Date"), "Tue, 1 Mar 2022 10:00:00 +0000");
    assert!(header(&mail, "Message-ID").starts_with('<'));
    assert_eq!(mail.ctype.mimetype, "text/plain");
    assert_eq!(mail.get_body().unwrap().trim_end(), "hello there");
    // 纯 ASCII 正文保持可读，不平白套一层 base64。
    assert!(raw.contains("Content-Transfer-Encoding: 7bit"));
    assert!(raw.contains("hello there"));
}

#[test]
fn non_ascii_subject_and_body_survive_a_round_trip() {
    let message = base("周会纪要与后续行动项", "第一行\n第二行：中文正文");
    let raw = compose::assemble(&message);
    // 报文本身一定是纯 ASCII——因此它能原样进 TEXT 列（ADR-0028 决策 6）。
    assert!(raw.is_ascii(), "组装结果必须是纯 ASCII");
    assert!(!raw.contains("周会"), "中文不应以裸 8bit 出现");
    let mail = parsed(&raw);
    assert_eq!(header(&mail, "Subject"), "周会纪要与后续行动项");
    assert_eq!(
        mail.get_body().unwrap().replace("\r\n", "\n").trim_end(),
        "第一行\n第二行：中文正文"
    );
}

#[test]
fn a_long_non_ascii_subject_folds_without_splitting_characters() {
    let subject = "周会纪要".repeat(30);
    let raw = compose::assemble(&base(&subject, "body"));
    assert!(raw.is_ascii());
    // 折行后每行都在 RFC 5322 的硬限之内。
    assert!(
        raw.lines().all(|line| line.len() <= 998),
        "折行后仍有超长行"
    );
    assert_eq!(header(&parsed(&raw), "Subject"), subject);
}

// —— Bcc 边界 ——

#[test]
fn bcc_reaches_the_envelope_and_never_the_headers() {
    let mut message = base("Quarterly", "text");
    message.cc = vec!["cc@example.test".into()];
    message.bcc = vec!["secret@example.test".into()];
    let raw = compose::assemble(&message);
    assert!(
        !raw.contains("secret@example.test"),
        "密送地址不得出现在报文的任何位置:\n{raw}"
    );
    let mail = parsed(&raw);
    assert_eq!(header(&mail, "Cc"), "cc@example.test");
    assert_eq!(header(&mail, "Bcc"), "");
    // 但它必须在信封收件人里，否则根本收不到。
    assert_eq!(
        message.envelope_recipients(),
        vec![
            "alice@example.test".to_string(),
            "cc@example.test".to_string(),
            "secret@example.test".to_string(),
        ]
    );
}

#[test]
fn envelope_recipients_are_deduplicated_in_order() {
    let mut message = base("s", "b");
    message.to = vec!["a@example.test".into(), "b@example.test".into()];
    message.cc = vec!["a@example.test".into()];
    message.bcc = vec!["b@example.test".into(), "c@example.test".into()];
    assert_eq!(
        message.envelope_recipients(),
        vec![
            "a@example.test".to_string(),
            "b@example.test".to_string(),
            "c@example.test".to_string()
        ]
    );
}

// —— MIME 结构 ——

#[test]
fn html_alternative_and_attachments_produce_a_parsable_tree() {
    let mut message = base("带附件", "纯文本备选");
    message.body_html = "<p>富文本 <b>正文</b></p>".into();
    message.attachments = vec![OutgoingAttachment {
        filename: "报告.pdf".into(),
        mime_type: "application/pdf".into(),
        content: b"%PDF-1.7 fake".to_vec(),
    }];
    let raw = compose::assemble(&message);
    assert!(raw.is_ascii());
    let mail = parsed(&raw);
    assert_eq!(mail.ctype.mimetype, "multipart/mixed");

    let mut parts = Vec::new();
    leaves(&mail, &mut parts);
    let kinds: Vec<&str> = parts.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(kinds, vec!["text/plain", "text/html", "application/pdf"]);
    assert_eq!(
        String::from_utf8_lossy(&parts[2].1),
        "%PDF-1.7 fake",
        "附件字节必须逐字节还原"
    );
    // 附件名也走 encoded-word。
    let attachment = mail
        .subparts
        .iter()
        .find(|part| part.ctype.mimetype == "application/pdf")
        .expect("附件 part");
    assert!(attachment
        .get_content_disposition()
        .params
        .contains_key("filename"));
}

#[test]
fn threading_headers_are_written_when_present() {
    let mut message = base("Re: 周会", "回复正文");
    message.in_reply_to = "parent@example.test".into();
    message.references = "<root@example.test> <parent@example.test>".into();
    let mail_raw = compose::assemble(&message);
    let mail = parsed(&mail_raw);
    assert_eq!(header(&mail, "In-Reply-To"), "<parent@example.test>");
    assert_eq!(
        header(&mail, "References"),
        "<root@example.test> <parent@example.test>"
    );
}

#[test]
fn message_id_inputs_are_sanitized_against_header_injection() {
    // 客户端提供的头部值不能带着 CRLF 走进报文。
    let injected = compose::sanitize_message_id("ok@example.test>\r\nBcc: victim@example.test");
    assert!(!injected.contains('\r') && !injected.contains('\n') && !injected.contains('>'));
    let refs = compose::sanitize_references("<a@x>\r\nX-Evil: 1 <b@x>");
    assert!(!refs.contains('\r') && !refs.contains('\n'));
    assert!(refs.contains("<a@x>"));
}

// —— 回复 / 转发成型 ——

fn stored(store: &Store, uid: &str) -> MailMessage {
    MailMessage {
        account: "acc".into(),
        uid: uid.into(),
        sender: "Alice <alice@example.test>".into(),
        recipient: "me@example.test, team@example.test".into(),
        cc: "carol@example.test, alias@example.test".into(),
        reply_to: String::new(),
        subject: "Re: 周会纪要".into(),
        body: "第一行\n第二行".into(),
        received_at: NOW,
        date_at: Some(NOW),
        message_id: "parent@example.test".into(),
        references: "<root@example.test>".into(),
        folder: "INBOX".into(),
        attachments: vec![
            MailAttachment {
                part_index: 2,
                filename: "报告.pdf".into(),
                mime_type: "application/pdf".into(),
                size: 13,
                content: Some(b"%PDF-1.7 fake".to_vec()),
                ..Default::default()
            },
            MailAttachment {
                part_index: 3,
                filename: "signature.png".into(),
                content_id: "sig@maild".into(),
                inline: true,
                size: 4,
                content: Some(b"png!".to_vec()),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
    .tap_insert(store)
}

trait TapInsert {
    fn tap_insert(self, store: &Store) -> Self;
}

impl TapInsert for MailMessage {
    fn tap_insert(self, store: &Store) -> Self {
        store.insert_message(&self).unwrap();
        self
    }
}

#[test]
fn reply_all_inherits_the_thread_and_drops_our_own_addresses() {
    let store = Store::open("").unwrap();
    store
        .upsert_folder("acc", "INBOX", "INBOX", Some(FolderRole::Inbox))
        .unwrap();
    let original = stored(&store, "10");

    let mine = vec!["me@example.test".to_string(), "alias@example.test".to_string()];
    let to = compose::without_self(compose::extract_addresses(&original.sender), &mine);
    let mut everyone = compose::extract_addresses(&original.recipient);
    everyone.extend(compose::extract_addresses(&original.cc));
    let cc = compose::without_self(everyone, &mine);

    assert_eq!(to, vec!["alice@example.test".to_string()]);
    assert_eq!(
        cc,
        vec!["team@example.test".to_string(), "carol@example.test".to_string()],
        "自己与自己的别名都不该被抄送"
    );
    assert_eq!(compose::reply_subject(&original.subject), "Re: 周会纪要");
    assert_eq!(
        compose::inherited_references(
            &original.references,
            &original.in_reply_to,
            &original.message_id
        ),
        "<root@example.test> <parent@example.test>"
    );
}

#[test]
fn forwarding_carries_the_real_attachments_but_not_inline_images() {
    let store = Store::open("").unwrap();
    store
        .upsert_folder("acc", "INBOX", "INBOX", Some(FolderRole::Inbox))
        .unwrap();
    stored(&store, "11");

    let fetched = store.get_message("acc", Some("INBOX"), "11").unwrap().unwrap();
    let forwardable: Vec<&MailAttachment> = fetched
        .attachments
        .iter()
        .filter(|attachment| !attachment.inline)
        .collect();
    assert_eq!(forwardable.len(), 1);
    assert_eq!(forwardable[0].filename, "报告.pdf");

    // 字节已经在库里，转发不需要任何上传（ADR-0028 决策 8）。
    let bytes = store
        .attachment_content("acc", "INBOX", "11", forwardable[0].part_index)
        .unwrap()
        .expect("附件字节已缓存");
    assert_eq!(bytes, b"%PDF-1.7 fake");
    assert_eq!(compose::forward_subject(&fetched.subject), "Fwd: 周会纪要");
}

// —— 归档到已发送 ——

#[test]
fn sent_append_is_queued_only_when_the_account_has_a_sent_folder() {
    let store = Store::open("").unwrap();
    store
        .upsert_folder("acc", "INBOX", "INBOX", Some(FolderRole::Inbox))
        .unwrap();
    let raw = compose::assemble(&base("归档", "正文"));

    // 没有已发送文件夹：不入队，也不凭空造一个。
    assert!(!store.queue_append_op("acc", "Sent", &raw, NOW).unwrap());
    assert!(store.list_pending_ops(Some("acc"), true, 10).unwrap().is_empty());

    store
        .upsert_folder("acc", "Sent", "Sent", Some(FolderRole::Sent))
        .unwrap();
    assert!(store.queue_append_op("acc", "Sent", &raw, NOW).unwrap());
    let ops = store.list_pending_ops(Some("acc"), true, 10).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].kind, OpKind::Append);
    assert_eq!(ops[0].folder, "Sent");
    assert_eq!(ops[0].payload, raw, "归档的必须是投递出去的同一份字节");
}

#[test]
fn an_append_whose_outcome_is_unknown_is_terminal() {
    let store = Store::open("").unwrap();
    store
        .upsert_folder("acc", "Sent", "Sent", Some(FolderRole::Sent))
        .unwrap();
    store
        .queue_append_op("acc", "Sent", "raw", NOW)
        .unwrap();
    let op = store.claim_next_op("acc", NOW).unwrap().unwrap();
    let settled = store
        .finish_op(&op, OpState::Unknown, "连接断开", NOW)
        .unwrap();
    assert_eq!(
        settled,
        OpState::Unknown,
        "APPEND 重放会在已发送里留下两份，必须交用户裁决"
    );
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

#[tokio::test(flavor = "current_thread")]
async fn a_queued_append_lands_in_the_sent_folder() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    server.add_mailbox("Sent", "\\Sent");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.store]
[plugins.store.config]
db_path = ""

[[accounts]]
account = "e2e"
server = "127.0.0.1"
port = {}
username = "dev"
password = "dev"
"#,
        server.imap_addr.port()
    );
    let mut assembly = maild::assemble(&ctx, &toml).expect("装配成功");
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
    wait_until("已发送文件夹被发现", Duration::from_secs(10), || {
        probe.folder_by_name("e2e", "Sent").unwrap().is_some()
    })
    .await;

    let raw = compose::assemble(&base("归档验收", "正文"));
    assert!(store.queue_append_op("e2e", "Sent", &raw, NOW).unwrap());
    accounts.snapshot()[0].2.wake_writes();

    wait_until("副本抵达已发送", Duration::from_secs(10), || {
        server.mailbox_uids("Sent").len() == 1
    })
    .await;
    let probe = store.clone();
    wait_until("op 落定", Duration::from_secs(10), || {
        probe.list_pending_ops(Some("e2e"), true, 10).unwrap().is_empty()
    })
    .await;
    assert!(
        server.command_log().iter().any(|line| line.contains("APPEND")),
        "应当走 APPEND: {:?}",
        server.command_log()
    );
    assembly.dispose().await;
}
