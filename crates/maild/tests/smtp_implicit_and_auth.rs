//! ADR-0042：SMTPS 465 隐式 TLS greeting、AUTH PLAIN / AUTH LOGIN 协商与
//! 邮箱服务商自动发现的端到端验收。
//!
//! 假服务器用自签证书：transport 走 `accept_invalid_certs = true`，probe 走 debug 构建下的
//! `MAILD_TEST_ACCEPT_INVALID_CERTS=1` 豁免（与 account_lifecycle 一致）。

mod common;

use std::time::Duration;

use common::fake_mail::{FakeMail, SmtpAuthMode, SmtpTlsMode};
use maild::account_catalog::SecurityMode;
use maild::autodiscover::{
    detect_from_address, discover_provider, is_public_ip, list_presets, validate_public_domain,
    AuthGuideKind, UsernameFormat,
};
use maild::compose::{OutgoingAttachment, OutgoingMessage};
use maild::probe::{probe_outgoing, OutgoingProbe, ProbeStage, AUTH_REJECTED_DETAIL};
use maild::transport::{Transport, TransportConfig};

const USER: &str = "alice@test.local";
const PASS: &str = "app-pass-0042";
const DEADLINE: Duration = Duration::from_secs(10);

fn trust_test_certs() {
    std::env::set_var("MAILD_TEST_ACCEPT_INVALID_CERTS", "1");
}

fn transport(server: &FakeMail, tls: &str, password: &str) -> Transport {
    Transport::from_config(TransportConfig {
        server: "127.0.0.1".into(),
        port: server.smtp_addr.port(),
        username: USER.into(),
        password: password.into(),
        tls: tls.into(),
        accept_invalid_certs: true,
    })
}

fn probe<'a>(server: &FakeMail, security: SecurityMode, secret: &'a [u8]) -> OutgoingProbe<'a> {
    OutgoingProbe {
        server: "127.0.0.1",
        port: server.smtp_addr.port(),
        security,
        username: USER,
        secret: Some(secret),
    }
}

fn rich_message() -> OutgoingMessage {
    OutgoingMessage {
        from: USER.into(),
        to: vec!["bob@test.local".into()],
        cc: vec!["carol@test.local".into()],
        subject: "465 隐式 TLS 投递验收：中文主题".into(),
        body: "你好，这是一封经 SMTPS 465 投递的测试邮件。\n.以点开头的行\n".into(),
        attachments: vec![OutgoingAttachment {
            filename: "报告.txt".into(),
            mime_type: "text/plain".into(),
            content: "附件内容 attachment payload".as_bytes().to_vec(),
        }],
        message_id: maild::compose::new_message_id(USER, 1_700_000_000),
        date_unix: 1_700_000_000,
        ..Default::default()
    }
}

fn mechanisms(server: &FakeMail) -> Vec<String> {
    server
        .smtp_auth_log()
        .into_iter()
        .map(|(mechanism, _)| mechanism)
        .collect()
}

// —— 1. SMTPS 465：TLS 握手后先读 220 greeting ——

#[tokio::test(flavor = "current_thread")]
async fn implicit_tls_probe_reads_greeting_before_ehlo() {
    trust_test_certs();
    let server = FakeMail::start_smtp(
        SmtpTlsMode::Implicit,
        SmtpAuthMode::Plain,
        Some((USER, PASS)),
    )
    .await
    .expect("隐式 TLS 假服务器启动");

    let outcome = probe_outgoing(
        probe(&server, SecurityMode::Implicit, PASS.as_bytes()),
        DEADLINE,
    )
    .await;
    assert!(outcome.ok, "465 探测应成功: {outcome:?}");
    assert_eq!(outcome.stage, ProbeStage::Ready);
    assert_eq!(server.smtp_auth_log(), vec![("PLAIN".to_string(), true)]);
    assert!(server.messages().is_empty(), "探测绝不投递邮件");

    // 无认证探测同样要先读 greeting。
    let anonymous = probe_outgoing(
        OutgoingProbe {
            username: "",
            secret: None,
            ..probe(&server, SecurityMode::Implicit, b"")
        },
        DEADLINE,
    )
    .await;
    assert!(anonymous.ok, "无认证 465 探测应成功: {anonymous:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn implicit_tls_transport_delivers_rich_message() {
    let server = FakeMail::start_smtp(
        SmtpTlsMode::Implicit,
        SmtpAuthMode::Plain,
        Some((USER, PASS)),
    )
    .await
    .expect("隐式 TLS 假服务器启动");

    let message = rich_message();
    let raw = transport(&server, "implicit", PASS)
        .send_message(&message)
        .await
        .expect("465 隐式 TLS 投递应成功");

    let delivered = server.messages();
    assert_eq!(delivered.len(), 1, "恰好投递一封");
    let delivered = String::from_utf8(delivered[0].clone()).unwrap();
    let normalized_raw = raw.replace("\r\n", "\n");
    assert_eq!(
        delivered.replace("\r\n", "\n").trim_end(),
        normalized_raw.trim_end(),
        "服务端收到的字节与组装报文一致（含 dot-stuffing 还原）"
    );
    assert!(
        delivered.contains("Subject: =?UTF-8?B?"),
        "中文主题按 RFC 2047 编码"
    );
    assert!(
        delivered.contains("multipart/mixed"),
        "带附件为 multipart/mixed"
    );
    assert_eq!(server.smtp_auth_log(), vec![("PLAIN".to_string(), true)]);
}

// —— 2. AUTH LOGIN 回退 ——

#[tokio::test(flavor = "current_thread")]
async fn login_only_servers_fall_back_to_auth_login() {
    trust_test_certs();
    for (tls, security, tls_name, mode) in [
        (
            SmtpTlsMode::Implicit,
            SecurityMode::Implicit,
            "implicit",
            SmtpAuthMode::LoginOnly,
        ),
        (
            SmtpTlsMode::StartTls,
            SecurityMode::StartTls,
            "starttls",
            SmtpAuthMode::LoginOnly,
        ),
        (
            SmtpTlsMode::Implicit,
            SecurityMode::Implicit,
            "implicit",
            SmtpAuthMode::LegacyLoginOnly,
        ),
        (
            SmtpTlsMode::StartTls,
            SecurityMode::StartTls,
            "starttls",
            SmtpAuthMode::LegacyLoginOnly,
        ),
    ] {
        let server = FakeMail::start_smtp(tls, mode, Some((USER, PASS)))
            .await
            .expect("LOGIN-only 假服务器启动");

        let outcome = probe_outgoing(probe(&server, security, PASS.as_bytes()), DEADLINE).await;
        assert!(
            outcome.ok,
            "{mode:?}/{tls:?} 探测应回退 AUTH LOGIN: {outcome:?}"
        );

        transport(&server, tls_name, PASS)
            .send_message(&rich_message())
            .await
            .unwrap_or_else(|e| panic!("{mode:?}/{tls:?} 投递应回退 AUTH LOGIN: {e}"));
        assert_eq!(server.messages().len(), 1);
        assert_eq!(
            server.smtp_auth_log(),
            vec![("LOGIN".to_string(), true), ("LOGIN".to_string(), true)],
            "{mode:?}: 不得尝试未宣告的 PLAIN"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn plain_is_preferred_when_both_mechanisms_are_advertised() {
    trust_test_certs();
    let server = FakeMail::start_smtp(
        SmtpTlsMode::Implicit,
        SmtpAuthMode::PlainAndLogin,
        Some((USER, PASS)),
    )
    .await
    .expect("PLAIN+LOGIN 假服务器启动");
    let outcome = probe_outgoing(
        probe(&server, SecurityMode::Implicit, PASS.as_bytes()),
        DEADLINE,
    )
    .await;
    assert!(outcome.ok, "{outcome:?}");
    transport(&server, "implicit", PASS)
        .send_message(&rich_message())
        .await
        .expect("投递成功");
    assert_eq!(mechanisms(&server), vec!["PLAIN", "PLAIN"]);
}

#[tokio::test(flavor = "current_thread")]
async fn wrong_password_is_reported_as_auth_rejection_without_leaking() {
    trust_test_certs();
    for mode in [SmtpAuthMode::LoginOnly, SmtpAuthMode::Plain] {
        let server = FakeMail::start_smtp(SmtpTlsMode::Implicit, mode, Some((USER, PASS)))
            .await
            .expect("假服务器启动");
        let wrong = "wrong-secret-must-not-leak";

        let outcome = probe_outgoing(
            probe(&server, SecurityMode::Implicit, wrong.as_bytes()),
            DEADLINE,
        )
        .await;
        assert!(!outcome.ok, "{mode:?}: 错误密码必须失败");
        assert_eq!(outcome.stage, ProbeStage::Auth, "{mode:?}: {outcome:?}");
        assert_eq!(outcome.detail, AUTH_REJECTED_DETAIL);
        assert!(outcome.detail.contains("授权码"));

        let error = transport(&server, "implicit", wrong)
            .send_message(&rich_message())
            .await
            .expect_err("错误密码投递必须失败");
        assert!(
            error.contains("AUTH") && error.contains("535"),
            "{mode:?}: {error}"
        );
        assert!(!error.contains(wrong), "错误不得回显密码: {error}");
        assert!(server.messages().is_empty(), "认证失败不得投递");
        assert!(server.smtp_auth_log().iter().all(|(_, ok)| !ok));
    }
}

// —— 端口 / 加密方式配反的诊断 ——

#[tokio::test(flavor = "current_thread")]
async fn implicit_against_starttls_server_fails_at_tls_with_hint() {
    trust_test_certs();
    let server = FakeMail::start_smtp(SmtpTlsMode::StartTls, SmtpAuthMode::Plain, None)
        .await
        .expect("STARTTLS 假服务器启动");
    // 客户端发 ClientHello、服务端等一整行明文命令：要么握手立即失败，要么互等到 deadline，
    // 两种情况都必须停在 TLS 阶段并给出端口/加密方式提示。
    let outcome = probe_outgoing(
        probe(&server, SecurityMode::Implicit, PASS.as_bytes()),
        Duration::from_secs(2),
    )
    .await;
    assert!(!outcome.ok);
    assert_eq!(outcome.stage, ProbeStage::Tls, "{outcome:?}");
    assert!(
        outcome
            .detail
            .contains("993/465 通常使用 TLS，143/587 通常使用 STARTTLS"),
        "应给出端口/加密方式提示: {}",
        outcome.detail
    );
    assert!(
        server.smtp_auth_log().is_empty(),
        "不得在错位的连接上发送凭据"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn starttls_against_implicit_server_times_out_with_hint() {
    trust_test_certs();
    let server = FakeMail::start_smtp_implicit()
        .await
        .expect("隐式 TLS 假服务器启动");
    // 双方互等（客户端等明文 greeting，服务端等 ClientHello）：deadline 内给可行动提示。
    let outcome = probe_outgoing(
        probe(&server, SecurityMode::StartTls, PASS.as_bytes()),
        Duration::from_secs(2),
    )
    .await;
    assert!(!outcome.ok);
    assert_eq!(outcome.stage, ProbeStage::Tls, "{outcome:?}");
    assert!(
        outcome.detail.contains("TLS") && outcome.detail.contains("STARTTLS"),
        "{}",
        outcome.detail
    );
}

// —— 3. autodiscover ——

#[test]
fn autodiscover_matches_mainstream_providers() {
    let cases: &[(&str, &str, &str, u16, &str, u16, SecurityMode)] = &[
        (
            "alice@qq.com",
            "qq",
            "imap.qq.com",
            993,
            "smtp.qq.com",
            465,
            SecurityMode::Implicit,
        ),
        (
            "bob@foxmail.com",
            "qq",
            "imap.qq.com",
            993,
            "smtp.qq.com",
            465,
            SecurityMode::Implicit,
        ),
        (
            "c@163.com",
            "netease_163",
            "imap.163.com",
            993,
            "smtp.163.com",
            465,
            SecurityMode::Implicit,
        ),
        (
            "d@126.com",
            "netease_126",
            "imap.126.com",
            993,
            "smtp.126.com",
            465,
            SecurityMode::Implicit,
        ),
        (
            "e@gmail.com",
            "gmail",
            "imap.gmail.com",
            993,
            "smtp.gmail.com",
            465,
            SecurityMode::Implicit,
        ),
        (
            "f@googlemail.com",
            "gmail",
            "imap.gmail.com",
            993,
            "smtp.gmail.com",
            465,
            SecurityMode::Implicit,
        ),
        (
            "g@icloud.com",
            "icloud",
            "imap.mail.me.com",
            993,
            "smtp.mail.me.com",
            587,
            SecurityMode::StartTls,
        ),
        (
            "h@me.com",
            "icloud",
            "imap.mail.me.com",
            993,
            "smtp.mail.me.com",
            587,
            SecurityMode::StartTls,
        ),
        (
            "i@outlook.com",
            "outlook",
            "outlook.office365.com",
            993,
            "smtp-mail.outlook.com",
            587,
            SecurityMode::StartTls,
        ),
        (
            "j@hotmail.com",
            "outlook",
            "outlook.office365.com",
            993,
            "smtp-mail.outlook.com",
            587,
            SecurityMode::StartTls,
        ),
        (
            "k@fastmail.com",
            "fastmail",
            "imap.fastmail.com",
            993,
            "smtp.fastmail.com",
            465,
            SecurityMode::Implicit,
        ),
        (
            "l@proton.me",
            "proton_bridge",
            "127.0.0.1",
            1143,
            "127.0.0.1",
            1025,
            SecurityMode::StartTls,
        ),
        (
            "m@pm.me",
            "proton_bridge",
            "127.0.0.1",
            1143,
            "127.0.0.1",
            1025,
            SecurityMode::StartTls,
        ),
    ];
    for (input, id, imap, imap_port, smtp, smtp_port, smtp_security) in cases {
        let found = detect_from_address(input).unwrap_or_else(|| panic!("{input} 应命中预设"));
        assert_eq!(found.provider_id, *id, "{input}");
        assert_eq!(found.source, "preset");
        assert_eq!(found.confidence, "verified");
        assert_eq!(found.incoming.server, *imap, "{input}");
        assert_eq!(found.incoming.port, *imap_port, "{input}");
        assert_eq!(found.outgoing.server, *smtp, "{input}");
        assert_eq!(found.outgoing.port, *smtp_port, "{input}");
        assert_eq!(found.outgoing.security, *smtp_security, "{input}");
        assert_eq!(
            found.suggested_username, *input,
            "{input}: 用户名直接填完整地址"
        );
        assert_eq!(found.username_format, UsernameFormat::FullAddress);
        assert_eq!(found.mailbox, "INBOX");
        assert!(found.idle);
        assert!(found
            .help_url
            .as_deref()
            .is_some_and(|url| url.starts_with("https://")));
    }

    let qq = detect_from_address("Alice@VIP.QQ.com").unwrap();
    assert_eq!(qq.auth_kind, AuthGuideKind::AuthorizationCode);
    assert!(qq.strip_secret_spaces);
    assert_eq!(qq.suggested_username, "Alice@vip.qq.com");
    assert!(detect_from_address("gmail.com")
        .unwrap()
        .suggested_username
        .is_empty());
    assert_eq!(
        detect_from_address("a@gmail.com").unwrap().auth_kind,
        AuthGuideKind::AppPassword
    );
    assert_eq!(
        detect_from_address("a@outlook.com").unwrap().auth_kind,
        AuthGuideKind::OAuth2RequiredNotice
    );
    assert_eq!(
        detect_from_address("a@proton.me").unwrap().auth_kind,
        AuthGuideKind::LocalBridge
    );
    assert_eq!(
        detect_from_address("a@vip.163.com")
            .unwrap()
            .incoming
            .server,
        "imap.vip.163.com"
    );
    assert_eq!(
        detect_from_address("a@zohomail.cn")
            .unwrap()
            .incoming
            .server,
        "imap.zoho.com.cn"
    );
    assert!(detect_from_address("a@unknown-provider.example.org").is_none());

    let presets = list_presets();
    for id in [
        "gmail",
        "icloud",
        "qq",
        "netease_163",
        "netease_126",
        "outlook",
        "fastmail",
        "proton_bridge",
        "tencent_exmail",
        "aliyun",
    ] {
        assert_eq!(
            presets.iter().filter(|card| card.provider_id == id).count(),
            1,
            "预设卡片 {id} 恰好一张"
        );
    }
    assert!(presets
        .iter()
        .all(|card| card.suggested_username.is_empty()));

    // 可序列化往返（供 API / Tauri 复用）。
    let json = serde_json::to_string(&qq).unwrap();
    assert!(
        json.contains("\"authorization_code\"") && json.contains("\"implicit\""),
        "{json}"
    );
    let back: maild::autodiscover::ProviderDiscovery = serde_json::from_str(&json).unwrap();
    assert_eq!(back, qq);
}

#[tokio::test]
async fn autodiscover_blocks_ssrf_targets() {
    for input in [
        "a@localhost",
        "a@127.0.0.1",
        "a@[127.0.0.1]",
        "a@[::1]",
        "a@10.0.0.8",
        "a@192.168.1.1",
        "a@169.254.169.254",
        "a@printer.local",
        "a@db.internal",
        "a@intranet",
        "a@mail.corp",
        "a@foo.localhost",
        "a@例子.测试",
    ] {
        let error = discover_provider(input, true)
            .await
            .expect_err(&format!("{input} 必须在任何网络动作前被拒绝"));
        assert!(!error.is_empty());
    }
    assert!(validate_public_domain("Example.ORG.").unwrap() == "example.org");
    for ip in [
        "127.0.0.1",
        "10.1.1.1",
        "172.20.0.1",
        "192.168.0.1",
        "169.254.1.1",
        "::1",
        "fd12::1",
        "fe80::1",
        "0.0.0.0",
        "::",
    ] {
        assert!(!is_public_ip(&ip.parse().unwrap()), "{ip}");
    }
}

#[tokio::test]
async fn autodiscover_falls_back_to_heuristic_without_network() {
    let preset = discover_provider("alice@163.com", false).await.unwrap();
    assert_eq!(preset.provider_id, "netease_163");

    let guessed = discover_provider("ops@acme-mail.example.org", false)
        .await
        .unwrap();
    assert_eq!(guessed.provider_id, "custom");
    assert_eq!(guessed.source, "heuristic");
    assert_eq!(guessed.confidence, "guessed");
    assert_eq!(guessed.incoming.server, "imap.acme-mail.example.org");
    assert_eq!(
        (guessed.incoming.port, guessed.incoming.security),
        (993, SecurityMode::Implicit)
    );
    assert_eq!(guessed.outgoing.server, "smtp.acme-mail.example.org");
    assert_eq!(
        (guessed.outgoing.port, guessed.outgoing.security),
        (465, SecurityMode::Implicit)
    );
    assert_eq!(guessed.suggested_username, "ops@acme-mail.example.org");
}
