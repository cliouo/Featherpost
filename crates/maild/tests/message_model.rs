//! M4.5-01 消息模型验收（ADR-0025）：`Date` 解析与排序、HTML → 文本、
//! HTML 原文与附件落库、列表/详情/通知的载荷边界、协议 additive。

use maild::events::MailMessage;
use maild::mime;
use maild::receive::{parse_message, parse_message_at};
use maild::store::Store;

/// 2022-03-01T10:00:00Z
const DATE_HEADER_SECS: u64 = 1_646_128_800;
/// 观测时间取在 Date 之后一天，模拟「先发后收」。
const OBSERVED_AT: u64 = DATE_HEADER_SECS + 86_400;

fn raw(headers: &str, body: &str) -> Vec<u8> {
    format!("{headers}\r\n\r\n{body}\r\n").into_bytes()
}

// —— Date 头与排序键 ——

#[test]
fn date_header_is_parsed_and_drives_the_sort_key() {
    let message = parse_message_at(
        "acc",
        "1",
        &raw(
            "From: a@b\r\nSubject: dated\r\nDate: Tue, 1 Mar 2022 10:00:00 +0000",
            "hi",
        ),
        OBSERVED_AT,
    );
    assert_eq!(message.date_at, Some(DATE_HEADER_SECS));
    assert_eq!(message.received_at, OBSERVED_AT, "观测时间与发信时间是两回事");
    assert_eq!(message.sort_at, DATE_HEADER_SECS, "排序用发信时间");
}

#[test]
fn missing_or_unparsable_date_falls_back_to_observation_time() {
    for headers in [
        "From: a@b\r\nSubject: no date",
        "From: a@b\r\nSubject: junk date\r\nDate: 昨天下午",
    ] {
        let message = parse_message_at("acc", "1", &raw(headers, "hi"), OBSERVED_AT);
        assert_eq!(message.date_at, None, "{headers}");
        assert_eq!(message.sort_at, OBSERVED_AT, "{headers}");
    }
}

#[test]
fn forged_future_date_is_preserved_but_never_wins_the_sort() {
    // 伪造远未来的 Date 是常见垃圾邮件手法：会永久占据列表最上方。
    let message = parse_message_at(
        "acc",
        "1",
        &raw(
            "From: spam@example.test\r\nSubject: top\r\nDate: Fri, 1 Jan 2100 00:00:00 +0000",
            "buy",
        ),
        OBSERVED_AT,
    );
    assert!(message.date_at.is_some(), "发信人声称的时间如实保留");
    assert!(message.date_at.unwrap() > OBSERVED_AT);
    assert_eq!(message.sort_at, OBSERVED_AT, "不可信的 Date 不参与排序");
}

#[test]
fn messages_are_listed_by_send_time_not_insertion_order() {
    let store = Store::open("").unwrap();
    // 先插入较新的信，再插入较旧的信：插入序与发信序相反。
    for (uid, date_at) in [("newer", OBSERVED_AT - 10), ("older", OBSERVED_AT - 5_000)] {
        store
            .insert_message(&MailMessage {
                account: "acc".into(),
                uid: uid.into(),
                sender: "s@t".into(),
                subject: uid.into(),
                body: "x".into(),
                received_at: OBSERVED_AT,
                date_at: Some(date_at),
                ..Default::default()
            })
            .unwrap();
    }
    let uids: Vec<String> = store
        .list_message_summaries(Some("acc"), None, 10, 0)
        .unwrap()
        .into_iter()
        .map(|summary| summary.uid)
        .collect();
    assert_eq!(uids, vec!["newer".to_string(), "older".to_string()]);
}

#[test]
fn store_recomputes_the_sort_key_instead_of_trusting_the_payload() {
    let store = Store::open("").unwrap();
    store
        .insert_message(&MailMessage {
            account: "acc".into(),
            uid: "1".into(),
            sender: "s@t".into(),
            subject: "s".into(),
            body: "b".into(),
            received_at: OBSERVED_AT,
            date_at: Some(OBSERVED_AT + 10 * 86_400),
            // 载荷里的排序键是伪造的；落库必须自行判定。
            sort_at: u64::MAX,
            ..Default::default()
        })
        .unwrap();
    let stored = store.get_message("acc", None, "1").unwrap().unwrap();
    assert_eq!(stored.sort_at, OBSERVED_AT);
}

// —— HTML → 文本 ——

#[test]
fn style_and_script_contents_never_reach_the_body() {
    let html = "<html><head><style>.a{color:red}</style><title>标题</title></head>\
                <body><script>var secret='LEAKED';</script><p>正文 123456</p></body></html>";
    let message = parse_message_at(
        "acc",
        "1",
        &raw(
            "From: a@b\r\nSubject: html\r\nContent-Type: text/html; charset=utf-8",
            html,
        ),
        OBSERVED_AT,
    );
    assert!(message.body.contains("正文 123456"), "{:?}", message.body);
    for leaked in ["color:red", "LEAKED", "var secret", "标题"] {
        assert!(
            !message.body.contains(leaked),
            "{leaked:?} 不得进正文: {:?}",
            message.body
        );
        assert!(!message.snippet.contains(leaked), "{leaked:?} 不得进摘要");
    }
}

#[test]
fn tag_scanner_survives_attributes_comments_and_entities() {
    // 属性值里的 `>` 曾会截断标签，把属性内容漏成正文。
    let text = mime::html_to_text(
        "<a href=\"x?a=1&amp;b=2\" title=\"1 > 0\">link</a>\
         <!-- <p>comment body</p> -->&nbsp;tail&#65;&#x42;",
    );
    assert_eq!(text, "link tailAB");
}

#[test]
fn block_tags_break_lines_and_whitespace_is_collapsed() {
    let text = mime::html_to_text(
        "<p>one</p>\n\n   <div>two</div><br><span>three</span>\
         <table><tr><td>a</td><td>b</td></tr></table>",
    );
    // 块级元素的开闭标签各产生一次换行，归一化后段间恰好一个空行。
    assert_eq!(text, "one\n\ntwo\n\nthree\n\na b");
}

#[test]
fn unterminated_raw_text_element_does_not_swallow_the_body() {
    // 畸形 `<head>` 没有闭标签时，宁可多一点噪音也不要丢掉整封正文。
    let text = mime::html_to_text("<head><p>可见正文</p>");
    assert!(text.contains("可见正文"), "{text:?}");
}

// —— HTML 原文与载荷边界 ——

#[test]
fn html_original_is_kept_bounded_and_marked_when_truncated() {
    let large = format!("<div>{}</div>", "内容".repeat(120_000));
    assert!(large.len() > mime::MAX_STORED_HTML_BYTES);
    let message = parse_message_at(
        "acc",
        "1",
        &raw(
            "From: a@b\r\nSubject: big\r\nContent-Type: text/html; charset=utf-8",
            &large,
        ),
        OBSERVED_AT,
    );
    assert!(message.body_html.ends_with(mime::HTML_TRUNCATION_MARKER));
    assert!(
        message.body_html.len()
            <= mime::MAX_STORED_HTML_BYTES + mime::HTML_TRUNCATION_MARKER.len()
    );
    assert!(message.body.len() <= mime::MAX_STORED_TEXT_BYTES);
    assert!(message.snippet.chars().count() <= mime::SNIPPET_CHARS);
}

#[test]
fn list_path_omits_html_but_get_message_returns_it() {
    let store = Store::open("").unwrap();
    let message = parse_message_at(
        "acc",
        "1",
        &raw(
            "From: a@b\r\nSubject: html\r\nContent-Type: text/html; charset=utf-8",
            "<p>rendered</p>",
        ),
        OBSERVED_AT,
    );
    assert!(!message.body_html.is_empty());
    store.insert_message(&message).unwrap();

    let listed = store.list_messages(10, 0).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].body_html, "", "列表路径不返回 HTML 原文");
    assert_eq!(listed[0].body, "rendered");

    let fetched = store.get_message("acc", None, "1").unwrap().unwrap();
    assert_eq!(fetched.body_html, "<p>rendered</p>");

    // 空的重字段不进 JSON，旧客户端看到的形状不变。
    let listed_json = serde_json::to_value(&listed[0]).unwrap();
    assert!(listed_json.get("body_html").is_none());
    assert!(listed_json.get("attachments").is_none());
}

// —— 附件 ——

const BOUNDARY: &str = "MAILDBOUND";

fn multipart(parts: &[&str]) -> Vec<u8> {
    let mut out = String::from(
        "From: a@b\r\nSubject: parts\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"MAILDBOUND\"\r\n\r\n",
    );
    for part in parts {
        out.push_str(&format!("--{BOUNDARY}\r\n{part}\r\n"));
    }
    out.push_str(&format!("--{BOUNDARY}--\r\n"));
    out.into_bytes()
}

#[test]
fn attachments_are_retrievable_by_content_id() {
    let message = parse_message_at(
        "acc",
        "1",
        &multipart(&[
            "Content-Type: text/plain; charset=utf-8\r\n\r\nbody text",
            "Content-Type: image/png\r\nContent-ID: <logo@maild>\r\n\
             Content-Disposition: inline\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8=",
            "Content-Type: application/pdf; name=\"doc.pdf\"\r\n\
             Content-Disposition: attachment; filename=\"doc.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nd29ybGQ=",
        ]),
        OBSERVED_AT,
    );
    assert_eq!(message.body, "body text");
    assert_eq!(message.attachments.len(), 2);
    assert!(message.has_attachments, "存在非内联附件");

    let store = Store::open("").unwrap();
    store.insert_message(&message).unwrap();

    let listed = store.list_attachments("acc", "INBOX", "1").unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].content_id, "logo@maild");
    assert!(listed[0].inline);
    assert_eq!(listed[1].filename, "doc.pdf");
    assert_eq!(listed[1].mime_type, "application/pdf");
    assert!(!listed[1].inline);
    assert!(listed[0].content.is_none(), "列表路径不带字节");
    // ADR-0032 决策 3：`stored` 在解析时等于 `content.is_some()`，回读时等于 `content IS NOT NULL`。
    assert!(message.attachments.iter().all(|a| a.stored && a.content.is_some()));
    assert!(listed.iter().all(|a| a.stored), "列表路径如实带回 stored 位");

    let inline = store
        .attachment_by_content_id("acc", "INBOX", "1", "logo@maild")
        .unwrap()
        .expect("cid 命中");
    assert_eq!(inline.content.as_deref(), Some(b"hello".as_slice()));
    assert!(store
        .attachment_by_content_id("acc", "INBOX", "1", "absent@maild")
        .unwrap()
        .is_none());

    let by_part = store
        .attachment_content("acc", "INBOX", "1", listed[1].part_index)
        .unwrap();
    assert_eq!(by_part.as_deref(), Some(b"world".as_slice()));

    // getMessage 带回元数据，但不带字节。
    let fetched = store.get_message("acc", None, "1").unwrap().unwrap();
    assert_eq!(fetched.attachments.len(), 2);
    assert!(fetched.attachments.iter().all(|a| a.content.is_none()));

    // ADR-0032 决策 2/3：四元组读取的四种结果各有其名。
    use maild::store::AttachmentLookup;
    match store.read_attachment("acc", "INBOX", "1", listed[1].part_index).unwrap() {
        AttachmentLookup::Stored(part) => {
            assert_eq!(part.content.as_deref(), Some(b"world".as_slice()));
            assert_eq!(part.filename, "doc.pdf");
            assert!(part.stored);
        }
        other => panic!("应为 Stored，实际 {other:?}"),
    }
    assert!(matches!(
        store.read_attachment("acc", "INBOX", "1", 99).unwrap(),
        AttachmentLookup::PartMissing
    ));
    assert!(matches!(
        store.read_attachment("acc", "Sent", "1", listed[1].part_index).unwrap(),
        AttachmentLookup::MessageMissing
    ));
    assert!(matches!(
        store.read_attachment("other", "INBOX", "1", listed[1].part_index).unwrap(),
        AttachmentLookup::MessageMissing
    ));
}

#[test]
fn inline_image_alone_does_not_light_the_attachment_flag() {
    let message = parse_message_at(
        "acc",
        "1",
        &multipart(&[
            "Content-Type: text/plain\r\n\r\nsignature below",
            "Content-Type: image/png\r\nContent-ID: <sig@maild>\r\n\
             Content-Disposition: inline\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8=",
        ]),
        OBSERVED_AT,
    );
    assert_eq!(message.attachments.len(), 1);
    assert!(message.attachments[0].inline);
    assert!(
        !message.has_attachments,
        "正文里的签名图片不该点亮回形针"
    );
}

#[test]
fn oversized_attachment_keeps_metadata_without_inlining_bytes() {
    let payload = "A".repeat(mime::ATTACHMENT_INLINE_MAX_BYTES + 1);
    let part = format!(
        "Content-Type: application/octet-stream; name=\"big.bin\"\r\n\
         Content-Disposition: attachment; filename=\"big.bin\"\r\n\
         Content-Transfer-Encoding: 7bit\r\n\r\n{payload}"
    );
    let message = parse_message_at(
        "acc",
        "1",
        &multipart(&["Content-Type: text/plain\r\n\r\nsee attached", &part]),
        OBSERVED_AT,
    );
    let attachment = &message.attachments[0];
    assert_eq!(attachment.filename, "big.bin");
    assert!(
        attachment.size > mime::ATTACHMENT_INLINE_MAX_BYTES as u64,
        "尺寸如实记录: {}",
        attachment.size
    );
    assert!(attachment.content.is_none(), "超限只留元数据");
    assert!(!attachment.stored, "ADR-0032：元数据先说实话——没存就是 stored = false");

    let store = Store::open("").unwrap();
    store.insert_message(&message).unwrap();
    assert!(
        matches!(
            store
                .read_attachment("acc", "INBOX", "1", attachment.part_index)
                .unwrap(),
            maild::store::AttachmentLookup::NotStored(ref part) if !part.stored && part.size == attachment.size
        ),
        "超限 part 走 NotStored 分支（API 层映射 -32004）"
    );
    assert!(store
        .attachment_content("acc", "INBOX", "1", attachment.part_index)
        .unwrap()
        .is_none());
    assert_eq!(
        store.list_attachments("acc", "INBOX", "1").unwrap()[0].size,
        attachment.size,
        "元数据里的尺寸仍可信"
    );
}

// —— 协议 additive ——

#[test]
fn older_payloads_deserialize_into_the_extended_dtos() {
    // 旧 daemon → 新客户端：v3 之前的形状必须仍能反序列化。
    let legacy = serde_json::json!({
        "account": "acc",
        "uid": "1",
        "sender": "s@t",
        "subject": "old",
        "body": "text",
        "received_at": 7,
    });
    let message: maild_client::MailMessage = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(message.date_at, None);
    assert_eq!(message.sort_at, 0);
    assert!(message.attachments.is_empty());

    let summary: maild_client::MessageSummary = serde_json::from_value(legacy).unwrap();
    assert_eq!(summary.snippet, "");
    assert!(!summary.has_attachments);
}

#[test]
fn extended_payloads_survive_the_client_mirror_and_unknown_keys() {
    let message = parse_message_at(
        "acc",
        "1",
        &raw(
            "From: a@b\r\nSubject: s\r\nCc: c@d\r\nReply-To: r@d\r\n\
             Message-ID: <mid@example.test>\r\nIn-Reply-To: <parent@example.test>\r\n\
             References: <root@example.test> <parent@example.test>\r\n\
             Date: Tue, 1 Mar 2022 10:00:00 +0000",
            "body",
        ),
        OBSERVED_AT,
    );
    assert_eq!(message.message_id, "mid@example.test");
    assert_eq!(message.in_reply_to, "parent@example.test");
    assert_eq!(
        message.references,
        "<root@example.test> <parent@example.test>"
    );
    assert_eq!(message.cc, "c@d");
    assert_eq!(message.reply_to, "r@d");

    // 内核 DTO → 客户端镜像：字段一一对上。
    let mut encoded = serde_json::to_value(&message).unwrap();
    encoded["未来字段"] = serde_json::json!("新 daemon 加的键");
    let mirrored: maild_client::MailMessage = serde_json::from_value(encoded).unwrap();
    assert_eq!(mirrored.message_id, message.message_id);
    assert_eq!(mirrored.references, message.references);
    assert_eq!(mirrored.date_at, message.date_at);
    assert_eq!(mirrored.sort_at, message.sort_at);
}

// —— 既有行为不回归 ——

#[test]
fn plain_text_is_still_preferred_over_the_html_alternative() {
    let message = parse_message(
        "acc",
        "1",
        &multipart(&[
            "Content-Type: text/plain\r\n\r\nplain wins",
            "Content-Type: text/html\r\n\r\n<p>html body</p>",
        ]),
    );
    assert_eq!(message.body, "plain wins");
    assert_eq!(message.body_html, "<p>html body</p>", "HTML 原文同时保留");
    assert!(message.attachments.is_empty(), "alternative 正文不算附件");
    assert!(message.received_at > 0, "默认时钟仍取当下");
}
