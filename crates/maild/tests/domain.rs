//! R4 领域单元测试：otp 提取 / store 存取去重 / MIME 解析 / 配置合并。

use cordis::Service;
use maild::config::Merge;
use maild::events::MailMessage;
use maild::otp::extract_code;
use maild::receive::{parse_message, ReceiveConfig};
use maild::scheduler::SchedulerConfig;
use maild::store::Store;

// —— otp 提取 ——

#[test]
fn extract_default_six_digit() {
    assert_eq!(
        extract_code(r"\d{6}", "你的验证码是 123456，5 分钟内有效。", ""),
        Some("123456".into())
    );
}

#[test]
fn extract_prefers_body_then_subject() {
    assert_eq!(
        extract_code(r"\d{6}", "正文没有码", "主题 654321 验证码"),
        Some("654321".into())
    );
    // 正文有码时正文优先（haystack = body + "\n" + subject）
    assert_eq!(
        extract_code(r"\d{6}", "正文码 111111", "主题 222222"),
        Some("111111".into())
    );
}

#[test]
fn extract_no_code_returns_none() {
    assert_eq!(extract_code(r"\d{6}", "没有数字", "也没有"), None);
}

#[test]
fn extract_custom_pattern() {
    assert_eq!(
        extract_code(r"[A-Z]{3}-\d{4}", "code: ABC-1234 ok", ""),
        Some("ABC-1234".into())
    );
}

#[test]
fn extract_invalid_pattern_returns_none() {
    assert_eq!(extract_code("(unclosed", "x", "y"), None);
}

// —— store ——

fn msg(uid: &str, body: &str) -> MailMessage {
    MailMessage {
        account: "a".into(),
        uid: uid.into(),
        sender: "s@t".into(),
        recipient: String::new(),
        subject: "sub".into(),
        body: body.into(),
        received_at: 42,
        ..Default::default()
    }
}

#[test]
fn store_insert_and_dedupe() {
    let store = Store::open("").expect("内存库");
    assert!(
        store.insert_message(&msg("1", "hello")).unwrap(),
        "首次插入成功"
    );
    assert!(
        !store.insert_message(&msg("1", "hello")).unwrap(),
        "同 (account,uid) 幂等"
    );
    assert!(store.insert_message(&msg("2", "world")).unwrap());
    assert_eq!(store.count().unwrap(), 2);
}

#[test]
fn store_latest_roundtrip() {
    let store = Store::open("").expect("内存库");
    store.insert_message(&msg("10", "first")).unwrap();
    store.insert_message(&msg("20", "second")).unwrap();
    let latest = store.latest().unwrap().expect("有消息");
    assert_eq!(latest.uid, "20");
    assert_eq!(latest.body, "second");
    assert_eq!(latest.sender, "s@t");
}

#[test]
fn store_persists_to_file() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    {
        let store = Store::open(dir.to_str().unwrap()).expect("文件库");
        store.insert_message(&msg("7", "persisted")).unwrap();
    } // drop：连接关闭
    let store = Store::open(dir.to_str().unwrap()).expect("重开");
    assert_eq!(store.count().unwrap(), 1, "落盘后重开仍在");
}

// —— MIME 解析 ——

#[test]
fn parse_plain_message() {
    let raw = b"From: alice@example.com\r\nSubject: hi\r\n\r\nhello body\r\n";
    let m = parse_message("acc", "9", raw);
    assert_eq!(m.sender, "alice@example.com");
    assert_eq!(m.subject, "hi");
    assert_eq!(m.body, "hello body");
    assert_eq!(m.uid, "9");
    assert_eq!(m.account, "acc");
}

#[test]
fn parse_html_falls_back_stripped() {
    let raw = b"From: b@c\r\nSubject: html\r\nContent-Type: text/html\r\n\r\n<p>code <b>123456</b></p>\r\n";
    let m = parse_message("acc", "3", raw);
    assert!(
        m.body.contains("123456"),
        "html 降级去标签后仍含码: {:?}",
        m.body
    );
    assert!(!m.body.contains('<'), "标签已剥除: {:?}", m.body);
}

// —— 配置合并 ——

#[test]
fn receive_config_merge_field_override() {
    let base = ReceiveConfig {
        account: "main".into(),
        server: "imap.a.com".into(),
        port: 143,
        username: "u".into(),
        password: "p".into(),
        mailbox: "INBOX".into(),
        reconnect_secs: 5,
        idle: true,
        poll_secs: 60,
        folder_poll_secs: 300,
        tls: "none".into(),
        accept_invalid_certs: false,
        initial_sync_limit: 200,
    };
    let frag = ReceiveConfig {
        server: "imap.b.com".into(),
        tls: "starttls".into(),
        ..Default::default()
    };
    let merged = base.merge(&frag);
    assert_eq!(merged.server, "imap.b.com", "非空字段覆盖");
    assert_eq!(merged.port, 143, "零值不覆盖");
    assert_eq!(merged.account, "main");
    assert_eq!(merged.mailbox, "INBOX");
    assert!(merged.idle);
    assert_eq!(merged.reconnect_secs, 5);
    assert_eq!(merged.tls, "starttls", "tls 非空覆盖");
    assert_eq!(merged.folder_poll_secs, 300, "零值不覆盖 sweep 周期");
}

#[test]
fn config_types_are_services() {
    // 配置类型必须实现 Service（loader provide + 插件 inject 的前提）
    fn is_service<T: Service>() {}
    is_service::<ReceiveConfig>();
    is_service::<maild::otp::OtpConfig>();
    is_service::<maild::store::StoreConfig>();
    is_service::<maild::notify::NotifyConfig>();
    is_service::<SchedulerConfig>();
}

// —— otp 重放去重持久化（store 标记表） ——

#[test]
fn store_notified_markers_are_persistent() {
    let store = Store::open("").expect("内存库");
    assert!(!store.is_notified("a", "1").unwrap());
    assert!(store.mark_notified("a", "1").unwrap(), "首次标记");
    assert!(!store.mark_notified("a", "1").unwrap(), "重复标记幂等");
    assert!(store.is_notified("a", "1").unwrap());
    assert!(!store.is_notified("a", "2").unwrap(), "同账户不同 uid 独立");
    assert!(!store.is_notified("b", "1").unwrap(), "不同账户独立");
}

/// 模拟「重启 + 重放」：两个 OtpPlugin 实例（各自全新内存态）挂在同一上下文，
/// 同一封 (account, uid) 邮件被重放两次——持久化标记保证只通知一次。
#[test]
fn replayed_uid_notifies_only_once_across_plugin_instances() {
    use std::sync::Arc;

    use cordis::Context;
    use maild::events::{MailFiltered, MailMessage};
    use maild::notify::{Notifier, NotifyConfig};
    use maild::otp::{OtpConfig, OtpPlugin};

    let ctx = Context::new();
    let store = Arc::new(Store::open("").expect("内存库"));
    ctx.provide::<Store>(store);
    ctx.provide::<Notifier>(Arc::new(Notifier::new("dev-log").unwrap()));
    ctx.provide::<OtpConfig>(Arc::new(OtpConfig::default()));
    ctx.provide::<NotifyConfig>(Arc::new(NotifyConfig::default()));

    // 两个 otp 实例（模拟两个生命周期/重启后的新实例）
    ctx.plugin(Arc::new(OtpPlugin));
    ctx.plugin(Arc::new(OtpPlugin));

    let mail = MailMessage {
        account: "a".into(),
        uid: "7".into(),
        sender: "s@t".into(),
        recipient: String::new(),
        subject: "sub".into(),
        body: "验证码 777777".into(),
        received_at: 0,
        ..Default::default()
    };
    ctx.emit::<MailFiltered>(mail.clone());
    ctx.emit::<MailFiltered>(mail);

    let notifier = ctx.get::<Notifier>().unwrap();
    assert_eq!(
        notifier.sent().len(),
        1,
        "重放只通知一次: {:?}",
        notifier.sent()
    );
    assert!(notifier.sent()[0].body.contains("777777"));
}

/// otp 插件依赖 store：store 未提供时保持 Idle（响应式 inject）。
#[test]
fn otp_waits_for_store() {
    use std::sync::Arc;

    use cordis::{Context, FiberState};
    use maild::notify::Notifier;
    use maild::otp::{OtpConfig, OtpPlugin};

    let ctx = Context::new();
    ctx.provide::<Notifier>(Arc::new(Notifier::new("dev-log").unwrap()));
    ctx.provide::<OtpConfig>(Arc::new(OtpConfig::default()));
    let handle = ctx.plugin(Arc::new(OtpPlugin));
    assert_eq!(handle.state(), FiberState::Idle, "缺 store → 等待");

    ctx.provide::<Store>(Arc::new(Store::open("").unwrap()));
    assert_eq!(handle.state(), FiberState::Active, "store 就绪 → 激活");
}

// —— accounts 装配（[[accounts]] 配置校验） ——

#[test]
fn accounts_must_be_array_of_tables() {
    use cordis::Context;
    use maild::loader::LoadError;

    let ctx = Context::new();
    match maild::assemble(&ctx, "accounts = 1\n[plugins.notify]\n") {
        Err(LoadError::Structure(_)) => {}
        _ => panic!("accounts 非数组应报结构错误"),
    }
}

#[test]
fn account_missing_name_rejected() {
    use cordis::Context;
    use maild::loader::LoadError;

    let ctx = Context::new();
    match maild::assemble(
        &ctx,
        "[plugins.notify]\n[[accounts]]\nserver = \"imap.x\"\nport = 143\n",
    ) {
        Err(LoadError::Structure(_)) => {}
        _ => panic!("缺 account 名应报结构错误"),
    }
}

#[test]
fn duplicate_account_name_rejected() {
    use cordis::Context;
    use maild::loader::LoadError;

    let ctx = Context::new();
    let config = r#"
[[accounts]]
account = "main"
server = "imap.one"

[[accounts]]
account = "main"
server = "imap.two"
"#;
    match maild::assemble(&ctx, config) {
        Err(LoadError::Structure(detail)) => assert!(detail.contains("重复 account 名")),
        _ => panic!("重复 account 名应在分配 realm 前报结构错误"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn poll_account_requires_scheduler() {
    use cordis::Context;

    let ctx = Context::new();
    let config = r#"
[[accounts]]
account = "poll"
server = "127.0.0.1"
port = 9
idle = false
poll_secs = 1
"#;
    let asm = maild::assemble(&ctx, config).expect("顶层配置有效");
    assert_eq!(asm.errors.len(), 1, "轮询账户缺 scheduler 应装配失败");
    assert!(ctx.get::<maild::Accounts>().is_none());
}

#[test]
fn quiet_hours_requires_scheduler() {
    use cordis::Context;
    use maild::loader::LoadError;

    let ctx = Context::new();
    let config = r#"
[plugins.rules]
[plugins.rules.config]
quiet_hours = [{ start = "22:00", end = "07:00" }]
"#;
    let asm = maild::assemble(&ctx, config).expect("顶层配置有效");
    assert!(asm.errors.iter().any(|error| matches!(
        error,
        LoadError::Apply { plugin, detail }
            if plugin == "rules" && detail.contains("需要先装配 scheduler")
    )));
}

#[test]
fn otp_config_default_matches_serde_defaults() {
    // 手写 Default 与 serde 默认值必须一致（防双路径漂移，见 PROGRESS.md R4.5 备注）
    let d = maild::otp::OtpConfig::default();
    assert_eq!(d.pattern, r"\d{6}");
    assert_eq!(d.title, "验证码");

    let r = maild::receive::ReceiveConfig::default();
    assert_eq!(r.port, 143);
    assert_eq!(r.mailbox, "INBOX");
    assert_eq!(r.reconnect_secs, 5);
    assert!(r.idle);
    assert_eq!(r.poll_secs, 60);
    assert_eq!(r.tls, "none", "TLS 默认明文");
    assert!(!r.accept_invalid_certs, "默认校验证书");

    let scheduler = SchedulerConfig::default();
    assert_eq!(scheduler.jitter_secs, 1);
}
