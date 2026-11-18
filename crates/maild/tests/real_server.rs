//! 真服务器交叉验证：Stalwart Mail 0.15.5（独立实现）。
//!
//! 背景：hermetic e2e 打的是自建假服务器（同源实现偏见）。本测试用独立实现的
//! Stalwart（Rust 编写的生产级邮件服务器）交叉验证我们的客户端：
//! - transport（SMTP STARTTLS）→ Stalwart SMTP 收信
//! - receive（IMAP STARTTLS + IDLE）→ 登录 dev、收新邮件
//! - otp → 提取验证码 → notify 通知
//!
//! 前置：本地 Stalwart 已按 `.tools/stalwart/README.md` 启动（127.0.0.1:1025 SMTP
//! STARTTLS 免认证本地投递 / 127.0.0.1:1143 IMAP STARTTLS，账号 dev/devpass，自签证书）。
//! 运行：
//! ```sh
//! cargo test -p maild --test real_server -- --ignored --nocapture
//! ```
//! 可用环境变量覆盖：`REAL_IMAP_HOST/PORT/USER/PASS/TLS/ACCEPT_INVALID_CERTS`、
//! `REAL_SMTP_HOST/PORT/USER/PASS/TLS/ACCEPT_INVALID_CERTS/FROM/TO`。

use std::time::Duration;

use cordis::Context;
use maild::notify::Notifier;
use maild::transport::Transport;
use maild::Accounts;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| match value.as_str() {
            "true" => true,
            "false" => false,
            _ => panic!("{name} 必须是 true 或 false"),
        })
        .unwrap_or(default)
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "需本地 Stalwart 运行（见 .tools/stalwart/README.md）"]
async fn stalwart_receive_otp_notify() {
    let imap_host = env_or("REAL_IMAP_HOST", "127.0.0.1");
    let imap_port = env_or("REAL_IMAP_PORT", "1143");
    let imap_user = env_or("REAL_IMAP_USER", "dev");
    let imap_pass = env_or("REAL_IMAP_PASS", "devpass");
    let imap_tls = env_or("REAL_IMAP_TLS", "starttls");
    let imap_accept_invalid_certs = env_bool("REAL_IMAP_ACCEPT_INVALID_CERTS", true);
    let smtp_host = env_or("REAL_SMTP_HOST", "127.0.0.1");
    let smtp_port = env_or("REAL_SMTP_PORT", "1025");
    let smtp_user = env_or("REAL_SMTP_USER", "");
    let smtp_pass = env_or("REAL_SMTP_PASS", "");
    let smtp_tls = env_or("REAL_SMTP_TLS", "starttls");
    let smtp_accept_invalid_certs = env_bool("REAL_SMTP_ACCEPT_INVALID_CERTS", true);
    let mail_from = env_or("REAL_SMTP_FROM", "sender@test.local");
    let mail_to = env_or("REAL_SMTP_TO", "dev@maild.test");
    let otp_code = format!(
        "{:06}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间")
            .as_millis()
            % 1_000_000
    );

    let imap_host = toml_string(&imap_host);
    let imap_user = toml_string(&imap_user);
    let imap_pass = toml_string(&imap_pass);
    let imap_tls = toml_string(&imap_tls);
    let smtp_host = toml_string(&smtp_host);
    let smtp_user = toml_string(&smtp_user);
    let smtp_pass = toml_string(&smtp_pass);
    let smtp_tls = toml_string(&smtp_tls);

    let ctx = Context::new();
    let toml = format!(
        r#"
[plugins.transport]
[plugins.transport.config]
server = {smtp_host}
port = {smtp_port}
username = {smtp_user}
password = {smtp_pass}
tls = {smtp_tls}
accept_invalid_certs = {smtp_accept_invalid_certs}

[plugins.store]
[plugins.store.config]
db_path = ""

[plugins.notify]

[plugins.otp]

[[accounts]]
account = "real-server"
server = {imap_host}
port = {imap_port}
username = {imap_user}
password = {imap_pass}
tls = {imap_tls}
accept_invalid_certs = {imap_accept_invalid_certs}
"#
    );
    let mut asm = maild::assemble(&ctx, &toml).expect("装配成功");
    assert!(asm.errors.is_empty(), "装配错误: {:?}", asm.errors);

    // 等 receive 在线（STARTTLS + IDLE 就绪）
    let accounts = ctx.get::<Accounts>().expect("accounts 服务");
    assert_eq!(accounts.snapshot().len(), 1);
    tokio::time::timeout(Duration::from_secs(15), accounts.snapshot()[0].2.wait_online())
        .await
        .expect("Stalwart STARTTLS 通道 15s 内在线");

    // transport（SMTP STARTTLS）发一封验证码邮件到 dev@maild.test
    let transport = ctx.get::<Transport>().expect("transport 服务");
    transport
        .send(
            &mail_from,
            &mail_to,
            "登录验证码",
            &format!("你的验证码是 {otp_code}，5 分钟内有效。"),
        )
        .await
        .expect("SMTP 发信成功");

    // 等 otp 通知（IMAP IDLE 推送 → 抓取 → 提取 → 通知）
    let notifier = ctx.get::<Notifier>().expect("notifier 服务");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut notifications = Vec::new();
    while tokio::time::Instant::now() < deadline {
        notifications = notifier.sent();
        if notifications
            .iter()
            .any(|notification| notification.body.contains(&otp_code))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        notifications
            .iter()
            .any(|notification| notification.title == "验证码"
                && notification.body.contains(&otp_code)),
        "通知应含本次验证码 {otp_code}: {notifications:?}",
    );

    asm.dispose().await;
}
