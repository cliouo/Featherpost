//! R4.5 e2e：本地假邮件服务器跑通「收信→验证码→系统通知」+ 多账户事件隔离 + 轮询兜底。
//!
//! 完全 hermetic：假服务器监听 ephemeral 端口（`tests/common/fake_mail.rs`），
//! 不依赖外部进程（Mailpit 无 IMAP，见 docs/PROGRESS.md 开放问题）。

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cordis::Context;
use maild::api::serve;
use maild::events::{MailFiltered, MailMessage, NotifyRoute, NotifyRouteEvent};
use maild::notify::Notifier;
use maild::store::Store;
use maild::Accounts;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use mailparse::MailHeaderMap as _;

use common::fake_mail::FakeMail;

/// 最小 SMTP 客户端：发一封含验证码的邮件。
async fn send_mail(addr: std::net::SocketAddr, code: &str, subject: &str) {
    let stream = TcpStream::connect(addr).await.expect("连上假 SMTP");
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();

    async fn read_code(
        lines: &mut tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
    ) -> String {
        loop {
            let l = lines.next_line().await.expect("响应行").expect("连接未断");
            // 多行响应：以「3 位码 + 空格」结尾的那行是终行
            if l.len() >= 4 && l.as_bytes()[3] == b' ' {
                return l;
            }
        }
    }

    assert!(read_code(&mut lines).await.starts_with("220"), "greeting");

    w.write_all(b"EHLO e2e\r\n").await.unwrap();
    read_code(&mut lines).await;
    w.write_all(b"MAIL FROM:<sender@test.local>\r\n")
        .await
        .unwrap();
    read_code(&mut lines).await;
    w.write_all(b"RCPT TO:<rcpt@test.local>\r\n").await.unwrap();
    read_code(&mut lines).await;
    w.write_all(b"DATA\r\n").await.unwrap();
    assert!(read_code(&mut lines).await.starts_with("354"), "DATA 许可");

    let body = format!("你的验证码是 {code}，5 分钟内有效。");
    w.write_all(b"From: sender@test.local\r\n").await.unwrap();
    w.write_all(b"To: rcpt@test.local\r\n").await.unwrap();
    w.write_all(format!("Subject: {subject}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    w.write_all(body.as_bytes()).await.unwrap();
    w.write_all(b"\r\n.\r\n").await.unwrap();
    assert!(read_code(&mut lines).await.starts_with("250"), "投递成功");

    w.write_all(b"QUIT\r\n").await.unwrap();
    read_code(&mut lines).await;
}

/// 轮询等待条件成立。
async fn wait_until<F: Fn() -> bool>(what: &str, deadline: Duration, f: F) {
    let until = tokio::time::Instant::now() + deadline;
    while tokio::time::Instant::now() < until {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("超时: {what}");
}

fn quiet_window_around_now() -> (String, String) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时间应在 Unix epoch 之后")
        .as_secs();
    let minute = ((now / 60) % 1_440) as i64;
    let format = |value: i64| {
        let value = value.rem_euclid(1_440);
        format!("{:02}:{:02}", value / 60, value % 60)
    };
    (format(minute - 1), format(minute + 2))
}

#[tokio::test(flavor = "current_thread")]
async fn receive_otp_notify_e2e() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");

    // —— 与守护进程同一条装配路径（assemble：全局插件 + [[accounts]]） ——
    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "e2e"
server = "127.0.0.1"
port = {}
username = "dev"
password = "dev"
"#,
        server.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);
    assert_eq!(
        asm.report()
            .iter()
            .filter(|(_, s)| *s != cordis::FiberState::Active)
            .count(),
        0,
        "全部插件 Active: {:?}",
        asm.report()
    );

    // —— 等 receive 在线（IDLE 就绪），避免发信早于订阅 ——
    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    assert_eq!(accounts.snapshot().len(), 1);
    tokio::time::timeout(Duration::from_secs(10), accounts.snapshot()[0].2.wait_online())
        .await
        .expect("10s 内未在线");

    // —— SMTP 发一封验证码邮件 ——
    send_mail(server.smtp_addr, "246810", "登录验证码").await;

    // —— 等通知 ——
    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    wait_until("通知出现", Duration::from_secs(10), || {
        !notifier.sent().is_empty()
    })
    .await;
    let notifications = notifier.sent();
    assert_eq!(notifications.len(), 1, "恰好一条通知: {notifications:?}");
    assert_eq!(notifications[0].title, "验证码");
    assert!(
        notifications[0].body.contains("246810"),
        "通知应含验证码: {:?}",
        notifications[0].body
    );

    // —— store 落库 ——
    let store = ctx.get::<Store>().expect("store 服务");
    wait_until("store 落库", Duration::from_secs(5), || {
        store.count().unwrap_or(0) == 1
    })
    .await;
    let latest = store.latest().unwrap().expect("有消息");
    assert_eq!(latest.subject, "登录验证码");
    assert!(latest.body.contains("246810"));

    // —— 卸载：LIFO，长连接干净关闭 ——
    asm.dispose().await;
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn multi_account_events_isolated() {
    let server_a = FakeMail::start().await.expect("假服务器 A");
    let server_b = FakeMail::start().await.expect("假服务器 B");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "alpha"
server = "127.0.0.1"
port = {}
username = "u"
password = "p"

[[accounts]]
account = "beta"
server = "127.0.0.1"
port = {}
username = "u"
password = "p"
"#,
        server_a.imap_addr.port(),
        server_b.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    assert_eq!(accounts.snapshot().len(), 2);
    // ROOT 上没有共享 Receive（isolate 语义：每账户独立绑定）
    assert!(
        ctx.get::<maild::receive::Receive>().is_none(),
        "ROOT 无 Receive"
    );

    // 账户 alpha 的 scope 上挂探针：只见本账户事件（R2 事件 scope 实战验证）
    let snapshot = accounts.snapshot();
    let (name_a, realm_a, _) = &snapshot[0];
    assert_eq!(name_a, "alpha");
    let probe_ctx = ctx.scoped(*realm_a);
    let seen_a: Arc<Mutex<Vec<MailMessage>>> = Arc::default();
    let s = Arc::clone(&seen_a);
    let _probe = probe_ctx.on_emit::<MailFiltered>(move |msg| {
        s.lock().unwrap().push(msg);
    });

    // 等两个账户都在线
    for (name, _, rx) in accounts.snapshot() {
        tokio::time::timeout(Duration::from_secs(10), rx.wait_online())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "账户 {name} 10s 内未在线；状态 {:?} / {:?}",
                    rx.state(),
                    accounts
                        .snapshot()
                        .iter()
                        .map(|(n, _, r)| (n.clone(), r.state()))
                        .collect::<Vec<_>>()
                )
            });
    }

    // 各自发一封验证码邮件
    send_mail(server_a.smtp_addr, "111111", "登录验证码").await;
    send_mail(server_b.smtp_addr, "222222", "登录验证码").await;

    // 全局：两条通知、两条落库
    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    wait_until("两条通知", Duration::from_secs(10), || {
        notifier.sent().len() == 2
    })
    .await;
    let bodies: Vec<String> = notifier.sent().iter().map(|n| n.body.clone()).collect();
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("111111") && b.contains("alpha")),
        "alpha 的通知: {bodies:?}"
    );
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("222222") && b.contains("beta")),
        "beta 的通知: {bodies:?}"
    );

    let store = ctx.get::<Store>().expect("store 服务");
    wait_until("两条落库", Duration::from_secs(5), || {
        store.count().unwrap_or(0) == 2
    })
    .await;

    // 隔离断言：alpha 的 scope 探针只看到 alpha 的邮件
    wait_until(
        "alpha 探针收到本账户邮件",
        Duration::from_secs(5),
        || seen_a.lock().unwrap().len() == 1,
    )
    .await;
    {
        let seen = seen_a.lock().unwrap();
        assert_eq!(seen.len(), 1, "探针只应见本账户一封: {seen:?}");
        assert_eq!(seen[0].account, "alpha");
        assert!(seen[0].body.contains("111111"));
    }

    asm.dispose().await;
    drop((server_a, server_b));
}

#[tokio::test(flavor = "current_thread")]
async fn poll_fallback_receives() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[plugins.scheduler]
[plugins.scheduler.config]
jitter_secs = 0

[[accounts]]
account = "poll"
server = "127.0.0.1"
port = {}
username = "dev"
password = "dev"
idle = false
poll_secs = 1
"#,
        server.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty());

    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(Duration::from_secs(10), accounts.snapshot()[0].2.wait_online())
        .await
        .expect("在线");

    send_mail(server.smtp_addr, "333333", "登录验证码").await;

    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    wait_until("轮询收到通知", Duration::from_secs(10), || {
        !notifier.sent().is_empty()
    })
    .await;
    assert!(
        notifier.sent()[0].body.contains("333333"),
        "轮询兜底收到验证码"
    );

    let fetches = server.imap_fetches();
    asm.dispose().await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(server.imap_fetches(), fetches, "卸载后 scheduler 停止轮询");
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn filters_drop_junk_mail() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.filters]
[plugins.filters.config]
[[plugins.filters.config.rules]]
field = "subject"
pattern = "广告"
action = "drop"

[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "filtered"
server = "127.0.0.1"
port = {}
username = "dev"
password = "dev"
"#,
        server.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(Duration::from_secs(10), accounts.snapshot()[0].2.wait_online())
        .await
        .expect("在线");

    // 垃圾邮件（主题含「广告」）+ 正常验证码邮件
    send_mail(server.smtp_addr, "135790", "双十一广告").await;
    send_mail(server.smtp_addr, "246810", "登录验证码").await;

    // 只有正常邮件通过过滤链：落库 1 条、通知 1 条
    let store = ctx.get::<Store>().expect("store 服务");
    wait_until("落库 1 条", Duration::from_secs(10), || {
        store.count().unwrap_or(0) == 1
    })
    .await;
    let latest = store.latest().unwrap().expect("有消息");
    assert!(
        latest.body.contains("246810"),
        "落库的是正常邮件: {:?}",
        latest.subject
    );

    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    wait_until("通知 1 条", Duration::from_secs(10), || {
        notifier.sent().len() == 1
    })
    .await;
    let bodies: Vec<String> = notifier.sent().iter().map(|n| n.body.clone()).collect();
    assert!(
        bodies.iter().all(|b| !b.contains("135790")),
        "垃圾邮件不应触发通知: {bodies:?}"
    );
    assert!(
        bodies.iter().any(|b| b.contains("246810")),
        "正常邮件触发通知: {bodies:?}"
    );

    // 垃圾邮件确实到达了 IMAP（只是被过滤链丢弃）
    assert_eq!(store.count().unwrap(), 1, "垃圾邮件不落库");

    asm.dispose().await;
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn transport_sends_mail() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.transport]
[plugins.transport.config]
server = "127.0.0.1"
port = {}
"#,
        server.smtp_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty());

    let transport = ctx
        .get::<maild::transport::Transport>()
        .expect("transport 服务");
    transport
        .send(
            "me@test.local",
            "you@test.local",
            "问候",
            "你好，这是一封测试邮件。",
        )
        .await
        .expect("发送成功");

    wait_until("假服务器收到邮件", Duration::from_secs(5), || {
        !server.messages().is_empty()
    })
    .await;
    let msgs = server.messages();
    assert_eq!(msgs.len(), 1, "收到 1 封: {}", msgs.len());
    // ADR-0028：中文主题与正文分别走 RFC 2047 与 base64，解回来必须与原文一致。
    let parsed = mailparse::parse_mail(&msgs[0]).expect("外发报文可解析");
    assert_eq!(
        parsed.headers.get_first_value("Subject").as_deref(),
        Some("问候")
    );
    assert!(
        parsed.get_body().unwrap().contains("这是一封测试邮件"),
        "正文完整"
    );

    asm.dispose().await;
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn transport_starttls_sends_mail() {
    let server = FakeMail::start_smtp_starttls()
        .await
        .expect("SMTP STARTTLS 假服务器启动");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.transport]
[plugins.transport.config]
server = "127.0.0.1"
port = {}
tls = "starttls"
accept_invalid_certs = true
"#,
        server.smtp_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty());

    let transport = ctx
        .get::<maild::transport::Transport>()
        .expect("transport 服务");
    transport
        .send(
            "me@test.local",
            "you@test.local",
            "STARTTLS 问候",
            "这封邮件经 STARTTLS 加密发送。\n.单点行\r\n..双点行",
        )
        .await
        .expect("STARTTLS 发送成功");

    let msgs = server.messages();
    assert_eq!(msgs.len(), 1, "收到 1 封: {}", msgs.len());
    let parsed = mailparse::parse_mail(&msgs[0]).expect("外发报文可解析");
    assert_eq!(
        parsed.headers.get_first_value("Subject").as_deref(),
        Some("STARTTLS 问候")
    );
    // 行首的点经 dot-stuffing 上线、再由接收侧还原，解出来必须与原文逐字符一致。
    // （dot-stuffing 本身的单元测试在 transport 模块内。）
    let body = parsed.get_body().expect("正文可解码").replace("\r\n", "\n");
    assert_eq!(
        body.trim_end(),
        "这封邮件经 STARTTLS 加密发送。\n.单点行\n..双点行",
        "正文往返必须逐字符一致"
    );

    asm.dispose().await;
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn transport_starttls_refuses_missing_capability_without_downgrade() {
    let server = FakeMail::start().await.expect("明文假邮件服务器启动");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.transport]
[plugins.transport.config]
server = "127.0.0.1"
port = {}
tls = "starttls"
accept_invalid_certs = true
"#,
        server.smtp_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty());

    let transport = ctx
        .get::<maild::transport::Transport>()
        .expect("transport 服务");
    let error = transport
        .send(
            "me@test.local",
            "you@test.local",
            "不能降级",
            "服务端未声明 STARTTLS 时不得明文发送。",
        )
        .await
        .expect_err("缺少 STARTTLS capability 必须失败");
    assert!(error.contains("未声明 STARTTLS"), "阶段错误明确: {error}");
    assert!(server.messages().is_empty(), "不得降级投递明文邮件");

    asm.dispose().await;
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn transport_starttls_rejects_invalid_certificate_by_default() {
    let server = FakeMail::start_smtp_starttls()
        .await
        .expect("SMTP STARTTLS 假服务器启动");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.transport]
[plugins.transport.config]
server = "127.0.0.1"
port = {}
username = "test-user"
password = "smtp-secret-must-not-leak"
tls = "starttls"
"#,
        server.smtp_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty());

    let transport = ctx
        .get::<maild::transport::Transport>()
        .expect("transport 服务");
    let error = transport
        .send(
            "me@test.local",
            "you@test.local",
            "证书校验",
            "自签证书默认必须被拒绝。",
        )
        .await
        .expect_err("默认系统证书校验必须拒绝自签证书");
    assert!(error.contains("SMTP TLS 握手失败"), "阶段错误明确: {error}");
    assert!(
        !error.contains("smtp-secret-must-not-leak"),
        "错误不得泄露 SMTP 密码: {error}"
    );
    assert!(server.messages().is_empty(), "握手失败不得投递邮件");

    asm.dispose().await;
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn rules_silence_sensitive_notifications() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.rules]
[plugins.rules.config]
show_sensitive = false

[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "quiet"
server = "127.0.0.1"
port = {}
username = "dev"
password = "dev"
"#,
        server.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(Duration::from_secs(10), accounts.snapshot()[0].2.wait_online())
        .await
        .expect("在线");

    send_mail(server.smtp_addr, "111222", "登录验证码").await;
    send_mail(server.smtp_addr, "333444", "机密验证码").await;

    // otp 提取仍在进行（落库 2 条），但敏感通知全部静默
    let store = ctx.get::<Store>().expect("store 服务");
    wait_until("两封落库", Duration::from_secs(10), || {
        store.count().unwrap_or(0) == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    assert!(
        notifier.sent().is_empty(),
        "敏感开关关闭 → 验证码通知全部静默: {:?}",
        notifier.sent()
    );

    asm.dispose().await;
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn rules_apply_failure_silences_notifications_fail_closed() {
    let ctx = Context::new();
    let mut asm = maild::assemble(
        &ctx,
        r#"
[plugins.rules]
[plugins.rules.config]
quiet_hours = [{ start = "22:00", end = "07:00" }]

[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]
[plugins.otp]
"#,
    )
    .expect("配置形状有效");
    assert!(
        asm.errors
            .iter()
            .any(|error| error.to_string().contains("rules")),
        "缺 scheduler 时 rules 必须明确装配失败: {:?}",
        asm.errors
    );
    assert!(ctx.get::<maild::rules::RulesConfig>().is_some());
    assert!(ctx.get::<maild::rules::RulesSettings>().is_none());

    ctx.emit::<MailFiltered>(MailMessage {
        account: "policy".into(),
        uid: "1".into(),
        sender: "sender@example.test".into(),
        recipient: String::new(),
        subject: "登录验证码".into(),
        body: "code 123456".into(),
        received_at: 1,
        ..Default::default()
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let store = ctx.get::<Store>().expect("store 服务");
    assert!(store.is_notified("policy", "1").unwrap());
    assert!(
        ctx.get::<Notifier>()
            .expect("notifier 服务")
            .sent()
            .is_empty(),
        "rules 未生效时敏感通知必须 fail-closed"
    );

    asm.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn rules_keyword_silences_only_matching() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.rules]
[plugins.rules.config]
silent_keywords = ["机密"]

[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "kw"
server = "127.0.0.1"
port = {}
username = "dev"
password = "dev"
"#,
        server.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty());

    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(Duration::from_secs(10), accounts.snapshot()[0].2.wait_online())
        .await
        .expect("在线");

    send_mail(server.smtp_addr, "111222", "机密验证码").await;
    send_mail(server.smtp_addr, "333444", "登录验证码").await;

    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    wait_until("恰好一条通知", Duration::from_secs(10), || {
        notifier.sent().len() == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let sent = notifier.sent();
    assert_eq!(sent.len(), 1, "只有非关键词邮件通知: {sent:?}");
    assert!(
        sent[0].body.contains("333444"),
        "关键词命中的被静默: {sent:?}"
    );

    asm.dispose().await;
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn rules_quiet_hours_silence_current_window() {
    let server = FakeMail::start().await.expect("假邮件服务器启动");
    let (start, end) = quiet_window_around_now();

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.rules]
[plugins.rules.config]
quiet_hours = [{{ start = "{start}", end = "{end}" }}]

[plugins.scheduler]
[plugins.scheduler.config]
jitter_secs = 0

[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "quiet-hours"
server = "127.0.0.1"
port = {port}
username = "dev"
password = "dev"
"#,
        port = server.imap_addr.port(),
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(Duration::from_secs(10), accounts.snapshot()[0].2.wait_online())
        .await
        .expect("在线");

    send_mail(server.smtp_addr, "555666", "窗口内验证码").await;
    let store = ctx.get::<Store>().expect("store 服务");
    wait_until("静默窗口邮件落库", Duration::from_secs(10), || {
        store.count().unwrap_or(0) == 1
    })
    .await;

    tokio::time::sleep(Duration::from_millis(300)).await;
    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    assert!(
        notifier.sent().is_empty(),
        "当前静默窗口不应发通知: {:?}",
        notifier.sent()
    );

    asm.dispose().await;
    drop(server);
}

// —— TLS（R4.5 Step 1：STARTTLS / 隐式 TLS 通道全链路） ——

use common::fake_mail::ImapTlsMode;

#[tokio::test(flavor = "current_thread")]
async fn starttls_receive_otp_notify() {
    let server = FakeMail::start_tls(ImapTlsMode::StartTls)
        .await
        .expect("假服务器（STARTTLS）");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "tls"
server = "127.0.0.1"
port = {}
username = "dev"
password = "dev"
tls = "starttls"
accept_invalid_certs = true
"#,
        server.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(Duration::from_secs(10), accounts.snapshot()[0].2.wait_online())
        .await
        .expect("STARTTLS 通道 10s 内在线");

    // SMTP 侧明文投递；收信全链路走 STARTTLS 升级后的加密通道
    send_mail(server.smtp_addr, "778899", "登录验证码").await;

    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    wait_until(
        "STARTTLS 通道收到通知",
        Duration::from_secs(10),
        || !notifier.sent().is_empty(),
    )
    .await;
    assert!(
        notifier.sent()[0].body.contains("778899"),
        "STARTTLS 通道跑通收信→验证码→通知: {:?}",
        notifier.sent()
    );

    asm.dispose().await;
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn implicit_tls_receive_otp_notify() {
    let server = FakeMail::start_tls(ImapTlsMode::Implicit)
        .await
        .expect("假服务器（隐式 TLS）");

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "tls993"
server = "127.0.0.1"
port = {}
username = "dev"
password = "dev"
tls = "implicit"
accept_invalid_certs = true
"#,
        server.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    tokio::time::timeout(Duration::from_secs(10), accounts.snapshot()[0].2.wait_online())
        .await
        .expect("隐式 TLS 通道 10s 内在线");

    send_mail(server.smtp_addr, "991100", "登录验证码").await;

    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    wait_until(
        "隐式 TLS 通道收到通知",
        Duration::from_secs(10),
        || !notifier.sent().is_empty(),
    )
    .await;
    assert!(
        notifier.sent()[0].body.contains("991100"),
        "隐式 TLS 通道跑通收信→验证码→通知: {:?}",
        notifier.sent()
    );

    asm.dispose().await;
    drop(server);
}

// —— api（M2 headless）：JSON-RPC over duplex 集成测试 ——

/// 在 duplex 上启动 `serve`，返回客户端流。
async fn spawn_api(ctx: Context) -> tokio::io::DuplexStream {
    let (client, server) = tokio::io::duplex(8192);
    let (server_read, server_write) = tokio::io::split(server);
    tokio::spawn(async move {
        serve(ctx, BufReader::new(server_read), server_write).await;
    });
    client
}

#[cfg(unix)]
fn unix_api_path(label: &str) -> std::path::PathBuf {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("md-{label}-{}-{suffix}.sock", std::process::id()))
}

/// 写一条 JSON-RPC 请求、读一条响应（行分隔）。
async fn rpc<R: tokio::io::AsyncBufRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    read: &mut R,
    write: &mut W,
    method: &str,
    params: Value,
    id: Value,
) -> Value {
    let req = json!({ "jsonrpc": "2.0", "method": method, "params": params, "id": id });
    write
        .write_all((req.to_string() + "\n").as_bytes())
        .await
        .unwrap();
    let mut buf = String::new();
    tokio::time::timeout(Duration::from_secs(5), read.read_line(&mut buf))
        .await
        .expect("读响应超时")
        .expect("读失败");
    serde_json::from_str(buf.trim()).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn api_status_and_send_over_duplex() {
    let server = FakeMail::start().await.expect("假邮件服务器");
    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.transport]
[plugins.transport.config]
server = "127.0.0.1"
port = {}
"#,
        server.smtp_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    let client = spawn_api(ctx.clone()).await;
    let (read, write) = tokio::io::split(client);
    let mut read = BufReader::new(read);
    let mut write = write;

    // status：初始 0 消息、无账户
    let r = rpc(&mut read, &mut write, "status", json!({}), json!(1)).await;
    assert_eq!(r["id"], json!(1));
    assert_eq!(r["result"]["message_count"], 0);
    assert_eq!(r["result"]["version"], env!("CARGO_PKG_VERSION"));

    // send：经 transport 投递到假 SMTP
    let r = rpc(
        &mut read,
        &mut write,
        "send",
        json!({ "from": "me@test.local", "to": "you@test.local", "subject": "hi", "body": "hello" }),
        json!(2),
    )
    .await;
    assert_eq!(r["result"]["ok"], true, "send 成功: {r}");

    // 假服务器确实收到
    wait_until("假服务器收到邮件", Duration::from_secs(5), || {
        !server.messages().is_empty()
    })
    .await;
    let msgs = server.messages();
    assert_eq!(msgs.len(), 1, "假服务器收到 1 封");
    let raw = String::from_utf8_lossy(&msgs[0]);
    assert!(raw.contains("Subject: hi"), "主题完整: {raw}");
    assert!(raw.contains("hello"), "正文完整");

    // 客户端 notification：执行 send，但不写响应。
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "send",
        "params": { "from": "me@test.local", "to": "you@test.local", "body": "fire-and-forget" }
    });
    write
        .write_all((notification.to_string() + "\n").as_bytes())
        .await
        .unwrap();
    wait_until("notification 发信", Duration::from_secs(5), || {
        server.messages().len() == 2
    })
    .await;

    // 未知方法 → -32601；前一条 notification 没有留下响应。
    let r = rpc(&mut read, &mut write, "nope", json!({}), json!(3)).await;
    assert_eq!(r["error"]["code"], -32601);

    asm.dispose().await;
    drop(server);
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn api_unix_socket_supports_multiple_clients_push_and_shutdown() {
    use tokio::net::{UnixListener, UnixStream};

    let ctx = Context::new();
    let path = unix_api_path("ipc");
    let listener = UnixListener::bind(&path).expect("绑定 hermetic Unix socket");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(maild::api::serve_unix(listener, ctx.clone(), async move {
        let _ = shutdown_rx.await;
    }));

    let client_a = UnixStream::connect(&path).await.expect("连接客户端 A");
    let client_b = UnixStream::connect(&path).await.expect("连接客户端 B");
    let (read_a, mut write_a) = tokio::io::split(client_a);
    let (read_b, mut write_b) = tokio::io::split(client_b);
    let mut read_a = BufReader::new(read_a);
    let mut read_b = BufReader::new(read_b);

    let status_a = rpc(&mut read_a, &mut write_a, "status", json!({}), json!(1)).await;
    let status_b = rpc(&mut read_b, &mut write_b, "status", json!({}), json!(2)).await;
    assert_eq!(status_a["result"]["message_count"], 0);
    // `runtime` 是实时内存读数（ADR-0041 决策 6），两次调用之间必然浮动：比较其余字段与
    // `runtime` 的键集合。
    let stable = |status: &Value| {
        let mut result = status["result"].clone();
        let runtime = result
            .as_object_mut()
            .and_then(|object| object.remove("runtime"))
            .expect("status.runtime");
        let keys: Vec<String> = runtime.as_object().unwrap().keys().cloned().collect();
        (result, keys)
    };
    assert_eq!(
        stable(&status_b),
        stable(&status_a),
        "IPC 会话共用 dispatch 语义"
    );

    ctx.emit::<MailFiltered>(MailMessage {
        account: "ipc".into(),
        uid: "42".into(),
        sender: "sender@example.test".into(),
        recipient: String::new(),
        subject: "socket push".into(),
        body: "pushed to every open connection".into(),
        received_at: 42,
        ..Default::default()
    });
    for reader in [&mut read_a, &mut read_b] {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .expect("读 IPC 推送超时")
            .expect("读 IPC 推送");
        let pushed: Value = serde_json::from_str(line.trim()).expect("推送 JSON");
        assert_eq!(pushed["method"], "message/received");
        assert_eq!(pushed["params"]["uid"], "42");
        assert!(pushed.get("id").is_none());
    }

    drop(read_a);
    drop(write_a);
    let client_c = UnixStream::connect(&path).await.expect("A 关闭后可重连 C");
    let (read_c, mut write_c) = tokio::io::split(client_c);
    let mut read_c = BufReader::new(read_c);
    let status_c = rpc(&mut read_c, &mut write_c, "status", json!({}), json!(3)).await;
    assert_eq!(status_c["id"], 3);

    shutdown_tx.send(()).expect("发送 IPC 关闭");
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("IPC server 关闭超时")
        .expect("IPC server task")
        .expect("IPC server 正常退出");
    assert!(!path.exists(), "server 退出后清理 socket 文件");

    let mut trailing = String::new();
    let count = tokio::time::timeout(Duration::from_secs(5), read_b.read_line(&mut trailing))
        .await
        .expect("等待已有 IPC 连接关闭超时")
        .expect("读关闭后 EOF");
    assert_eq!(count, 0, "优雅停机关闭已有会话");
}

#[tokio::test(flavor = "current_thread")]
async fn api_settings_update_rules_over_duplex() {
    let ctx = Context::new();
    let mut asm = maild::assemble(
        &ctx,
        r#"
[plugins.rules]
[plugins.rules.config]
show_sensitive = true
silent_keywords = ["existing"]
timezone_offset_minutes = 480
"#,
    )
    .expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    let client = spawn_api(ctx.clone()).await;
    let (read, mut write) = tokio::io::split(client);
    let mut read = BufReader::new(read);

    let initial = rpc(&mut read, &mut write, "getSettings", json!({}), json!(1)).await;
    assert_eq!(initial["result"]["rules"]["show_sensitive"], true);
    assert_eq!(
        initial["result"]["rules"]["silent_keywords"],
        json!(["existing"])
    );

    let updated = rpc(
        &mut read,
        &mut write,
        "setSettings",
        json!({ "rules": { "show_sensitive": false } }),
        json!(2),
    )
    .await;
    assert_eq!(updated["result"]["rules"]["show_sensitive"], false);
    assert_eq!(
        updated["result"]["rules"]["silent_keywords"],
        json!(["existing"]),
        "部分更新保留未提交字段"
    );
    assert_eq!(updated["result"]["rules"]["timezone_offset_minutes"], 480);
    assert_eq!(updated["result"]["persisted"], false);

    let route = ctx
        .bail::<NotifyRouteEvent>(NotifyRoute {
            channel: "system".into(),
            sensitive: true,
            subject: "普通验证码".into(),
            from: "sender@example.test".into(),
        })
        .expect("运行时设置应立即参与路由");
    assert_eq!(route.channel, "silent");

    drop(write);
    drop(read);
    asm.dispose().await;
}

#[tokio::test(flavor = "current_thread")]
async fn api_pushes_message_received_notification() {
    let server = FakeMail::start().await.expect("假邮件服务器");
    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "api"
server = "127.0.0.1"
port = {}
username = "dev"
password = "dev"
"#,
        server.imap_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    // 等 receive 在线（订阅就绪）后再起 api（先注册推送监听器再收信）
    let accounts = ctx.get::<maild::Accounts>().expect("accounts 服务");
    tokio::time::timeout(Duration::from_secs(10), accounts.snapshot()[0].2.wait_online())
        .await
        .expect("10s 内在线");

    let client = spawn_api(ctx.clone()).await;
    let (read, write) = tokio::io::split(client);
    let mut read = BufReader::new(read);

    // 发一封验证码邮件 → IMAP IDLE → MailFiltered → serve 推送 message/received
    send_mail(server.smtp_addr, "112233", "登录验证码").await;

    let mut buf = String::new();
    tokio::time::timeout(Duration::from_secs(10), read.read_line(&mut buf))
        .await
        .expect("推送超时")
        .expect("读失败");
    let v: Value = serde_json::from_str(buf.trim()).unwrap();
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["method"], "message/received");
    assert!(v.get("id").is_none(), "推送是无 id 的通知: {v}");
    assert!(
        v["params"]["body"].as_str().unwrap().contains("112233"),
        "推送正文含验证码: {v}"
    );

    // listMessages 能看到落库的消息
    let mut write = write;
    let r = rpc(&mut read, &mut write, "listMessages", json!({}), json!(1)).await;
    assert_eq!(r["result"]["total"], 1);
    assert_eq!(r["result"]["messages"][0]["account"], "api");

    // getMessage 用列表返回的账户和 UID 查询完整落库正文。
    let account = r["result"]["messages"][0]["account"]
        .as_str()
        .unwrap()
        .to_string();
    let uid = r["result"]["messages"][0]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let detail = rpc(
        &mut read,
        &mut write,
        "getMessage",
        json!({ "account": account, "uid": uid }),
        json!(2),
    )
    .await;
    assert_eq!(detail["result"]["message"]["account"], "api");
    assert_eq!(detail["result"]["message"]["uid"], uid);
    assert_eq!(
        detail["result"]["message"]["body"], v["params"]["body"],
        "详情返回完整持久化正文"
    );

    asm.dispose().await;
    drop(server);
}

#[test]
fn maild_binary_reserves_stdout_for_json_rpc() {
    use std::io::{BufRead as _, Write as _};
    use std::process::{Command, Stdio};

    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let config_path =
        std::env::temp_dir().join(format!("maild-api-{}-{suffix}.toml", std::process::id()));
    std::fs::write(
        &config_path,
        "[plugins.api]\n[plugins.api.config]\ntransport = \"stdio\"\n",
    )
    .unwrap();

    let mut child = Command::new(maild_brand::bin_exe_daemon!())
        .arg(&config_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动 maild 二进制");

    let stdout = child.stdout.take().expect("捕获 stdout");
    let (line_tx, line_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut stdout = std::io::BufReader::new(stdout);
        let mut line = String::new();
        let result = stdout.read_line(&mut line).map(|_| line);
        let _ = line_tx.send(result);
    });

    let mut stdin = child.stdin.take().expect("捕获 stdin");
    writeln!(
        stdin,
        "{}",
        json!({ "jsonrpc": "2.0", "method": "status", "id": 1 })
    )
    .unwrap();
    stdin.flush().unwrap();
    drop(stdin); // 一次性 CLI 写完即关输入，服务端仍须排空最后一条响应。

    let line = line_rx.recv_timeout(Duration::from_secs(5));
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    let _ = std::fs::remove_file(&config_path);

    let line = line.expect("5s 内收到 JSON-RPC 响应").expect("读取 stdout");
    let response: Value = serde_json::from_str(line.trim()).expect("stdout 首行必须是 JSON");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["message_count"], 0);
}

#[test]
fn maild_binary_exits_when_http_api_cannot_bind() {
    use std::process::Command;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let config_path = std::env::temp_dir().join(format!(
        "maild-api-bind-{}-{suffix}.toml",
        std::process::id()
    ));
    std::fs::write(
        &config_path,
        format!(
            "[plugins.api]\n[plugins.api.config]\ntransport = \"http\"\nlisten = \"{addr}\"\nbearer_token = \"{}\"\n",
            "t".repeat(32)
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let output = Command::new(maild_brand::bin_exe_daemon!())
        .arg(&config_path)
        .output()
        .expect("启动 maild 二进制");
    let _ = std::fs::remove_file(&config_path);
    drop(listener);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("api HTTP 插件未能启动"));
}

#[cfg(unix)]
#[test]
fn maild_binary_exits_when_unix_api_cannot_bind() {
    use std::os::unix::net::UnixListener;
    use std::process::Command;

    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("md-bind-{}-{suffix}", std::process::id()));
    std::fs::create_dir(&dir).expect("创建私有测试目录");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let socket_path = dir.join("occupied.sock");
    let listener = UnixListener::bind(&socket_path).expect("占用 Unix socket");
    let config_path = dir.join("bind-config.toml");
    std::fs::write(
        &config_path,
        format!(
            "[plugins.api]\n[plugins.api.config]\ntransport = \"unix\"\nlisten = {:?}\n",
            socket_path.to_string_lossy()
        ),
    )
    .unwrap();

    let output = Command::new(maild_brand::bin_exe_daemon!())
        .arg(&config_path)
        .output()
        .expect("启动 maild 二进制");
    let _ = std::fs::remove_file(&config_path);
    drop(listener);
    let _ = std::fs::remove_file(socket_path);
    let _ = std::fs::remove_dir(&dir);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("api Unix socket 插件未能启动"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn maildctl_calls_http_status_send_and_settings() {
    use tokio::process::Command;

    let server = FakeMail::start().await.expect("假邮件服务器");
    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.rules]
[plugins.rules.config]
show_sensitive = true

[plugins.transport]
[plugins.transport.config]
server = "127.0.0.1"
port = {}
"#,
        server.smtp_addr.port()
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token = "t".repeat(32);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let api_task = tokio::spawn(maild::api::serve_http(
        listener,
        ctx,
        token.clone(),
        async move {
            let _ = shutdown_rx.await;
        },
    ));
    let url = format!("http://{addr}");
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let config_path =
        std::env::temp_dir().join(format!("maildctl-{}-{suffix}.toml", std::process::id()));
    std::fs::write(
        &config_path,
        format!(
            "[plugins.api]\n[plugins.api.config]\ntransport = \"http\"\nlisten = \"{addr}\"\nbearer_token = \"{token}\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .args(["--json", "status"])
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .output()
        .await
        .expect("运行 maildctl status");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: Value = serde_json::from_slice(&output.stdout).expect("status JSON");
    assert_eq!(status["message_count"], 0);
    // ADR-0041 决策 6：低占用指标作为 additive `runtime` 对象可经 `maildctl status --json` 观测。
    let runtime = status["runtime"].as_object().expect("status.runtime");
    for key in [
        "rss_bytes",
        "phys_footprint_bytes",
        "store_db_bytes",
        "store_wal_bytes",
        "stored_messages",
        "stored_attachment_bytes",
    ] {
        assert!(runtime.contains_key(key), "缺少 runtime.{key}: {status}");
    }

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .args(["--json", "sync"])
        .output()
        .await
        .expect("运行 maildctl sync");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let synced: Value = serde_json::from_slice(&output.stdout).expect("sync JSON");
    assert!(synced["triggered"].is_array(), "{synced}");
    assert!(synced["online_count"].is_u64(), "{synced}");

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .args(["account", "discover", "someone@163.com"])
        .output()
        .await
        .expect("运行 maildctl account discover");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("imap: imap.163.com:993"), "stdout: {text}");
    assert!(text.contains("username: someone@163.com"), "stdout: {text}");

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .args(["--json", "settings"])
        .output()
        .await
        .expect("运行 maildctl settings");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let settings: Value = serde_json::from_slice(&output.stdout).expect("settings JSON");
    assert_eq!(settings["rules"]["show_sensitive"], true);

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .args([
            "set-settings",
            "--rules-json",
            r#"{"show_sensitive":false,"silent_keywords":["cli"]}"#,
        ])
        .output()
        .await
        .expect("运行 maildctl set-settings");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("show_sensitive: false"), "stdout: {text}");
    assert!(text.contains("persisted: false"), "stdout: {text}");

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .args(["--json", "settings"])
        .output()
        .await
        .expect("读取 maildctl 热更新 settings");
    let settings: Value = serde_json::from_slice(&output.stdout).expect("更新后的 settings JSON");
    assert_eq!(settings["rules"]["show_sensitive"], false);
    assert_eq!(settings["rules"]["silent_keywords"], json!(["cli"]));

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .arg("messages")
        .output()
        .await
        .expect("运行 maildctl RPC failure");
    assert_eq!(output.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("服务未装配"));

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .args([
            "send",
            "--from",
            "me@test.local",
            "--to",
            "you@test.local",
            "--subject",
            "cli",
            "--body",
            "from maildctl",
        ])
        .output()
        .await
        .expect("运行 maildctl send");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "sent");
    wait_until(
        "maildctl 发信到假服务器",
        Duration::from_secs(5),
        || !server.messages().is_empty(),
    )
    .await;
    let messages = server.messages();
    let raw = String::from_utf8_lossy(&messages[0]);
    assert!(raw.contains("Subject: cli"));
    assert!(raw.contains("from maildctl"));

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .args(["--url", &url, "status"])
        .env(maild_brand::TOKEN_ENV, "wrong-token-that-is-still-long-enough")
        .output()
        .await
        .expect("运行 maildctl auth failure");
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("鉴权失败"));

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("send")
        .output()
        .await
        .expect("运行 maildctl argument failure");
    assert_eq!(output.status.code(), Some(2));

    let _ = shutdown_tx.send(());
    api_task.await.unwrap().unwrap();
    let _ = std::fs::remove_file(config_path);
    asm.dispose().await;
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn maildctl_reads_message_detail_over_http() {
    use tokio::process::Command;

    let ctx = Context::new();
    let store = Arc::new(Store::open("").unwrap());
    store
        .insert_message(&MailMessage {
            account: "personal".into(),
            uid: "42".into(),
            sender: "sender@example.com".into(),
            recipient: String::new(),
            subject: "full detail".into(),
            body: "line one\nline two\n".into(),
            received_at: 1234,
            ..Default::default()
        })
        .unwrap();
    ctx.provide::<Store>(store);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let token = "d".repeat(32);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let api_task = tokio::spawn(maild::api::serve_http(
        listener,
        ctx,
        token.clone(),
        async move {
            let _ = shutdown_rx.await;
        },
    ));

    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let config_path = std::env::temp_dir().join(format!(
        "maildctl-detail-{}-{suffix}.toml",
        std::process::id()
    ));
    std::fs::write(
        &config_path,
        format!(
            "[plugins.api]\n[plugins.api.config]\ntransport = \"http\"\nlisten = \"{addr}\"\nbearer_token = \"{token}\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .env_remove(maild_brand::TOKEN_ENV)
        .env_remove(maild_brand::URL_ENV)
        .args(["message", "--account", "personal", "--uid", "42"])
        .output()
        .await
        .expect("运行 maildctl message");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        // ADR-0032：详情文本视图多一行策略结论；没装配策略服务时与缺省一致。
        "account: personal\nuid: 42\nfrom: sender@example.com\nsubject: full detail\nreceived_at: 1234\nremote_content: block (default)\n\nline one\nline two\n"
    );

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .env_remove(maild_brand::TOKEN_ENV)
        .env_remove(maild_brand::URL_ENV)
        .args(["--json", "message", "--account", "personal", "--uid", "42"])
        .output()
        .await
        .expect("运行 maildctl --json message");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let detail: Value = serde_json::from_slice(&output.stdout).expect("详情 JSON");
    assert_eq!(detail["message"]["account"], "personal");
    assert_eq!(detail["message"]["uid"], "42");
    assert_eq!(detail["message"]["body"], "line one\nline two\n");

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .arg("--config")
        .arg(&config_path)
        .env_remove(maild_brand::TOKEN_ENV)
        .env_remove(maild_brand::URL_ENV)
        .args(["message", "--account", "personal", "--uid", "missing"])
        .output()
        .await
        .expect("运行 maildctl missing message");
    assert_eq!(output.status.code(), Some(4));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("message not found (-32002)"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = Command::new(maild_brand::bin_exe_ctl!())
        .env_remove(maild_brand::TOKEN_ENV)
        .env_remove(maild_brand::URL_ENV)
        .args(["message", "--account", "personal"])
        .output()
        .await
        .expect("运行 maildctl missing required flag");
    assert_eq!(output.status.code(), Some(2));

    let _ = shutdown_tx.send(());
    api_task.await.unwrap().unwrap();
    let _ = std::fs::remove_file(config_path);
}
