//! ADR-0043：上下文感知验证码识别、普适新邮件通知与 `rules_state.toml` 原子持久化。

use std::path::Path;
use std::sync::Arc;

use cordis::Context;
use maild::events::{MailFiltered, MailMessage};
use maild::frontend::OpenIntent;
use maild::notify::{Notifier, NotifyConfig};
use maild::otp::{extract_code, extract_smart_otp, OtpConfig, OtpPlugin};
use maild::rules::{
    RulesConfig, RulesPatch, RulesPatchError, RulesPlugin, RulesSettings, RulesStatePath,
    RULES_STATE_FILE,
};
use maild::store::Store;

// —— 1. extract_smart_otp ——

#[test]
fn smart_otp_hits_common_chinese_and_english_templates() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "你的验证码是 246810，5 分钟内有效。",
            "登录验证码",
            "246810",
        ),
        ("验证码 777777", "sub", "777777"),
        ("【某银行】动态码：4821，请勿泄露。", "", "4821"),
        ("您的校验码为123456，10分钟内有效", "", "123456"),
        ("提取码: 8848", "分享文件", "8848"),
        (
            "Your verification code is 908172.",
            "Verify your email",
            "908172",
        ),
        (
            "Use 55443322 to sign in.",
            "Your one-time passcode",
            "55443322",
        ),
        ("Your OTP: 314159. Do not share it.", "", "314159"),
        ("Enter security code 2468 to continue", "", "2468"),
        ("Your 2FA code is 135790", "", "135790"),
        // 关键词只在主题、码在正文开头。
        (
            "135246\n\nThis code expires in 10 minutes.",
            "Login code",
            "135246",
        ),
        // 附近同时有电话与码：取离关键词最近、6 位优先的那个。
        ("验证码：4821。如有疑问请致电 400800", "", "4821"),
        // 年份与日期不被当作码。
        ("2026年11月17日 您的验证码是 902211", "", "902211"),
    ];
    for (body, subject, expected) in cases {
        assert_eq!(
            extract_smart_otp(body, subject).as_deref(),
            Some(*expected),
            "body={body:?} subject={subject:?}"
        );
    }
}

#[test]
fn smart_otp_never_fires_on_ordinary_mail() {
    let cases: &[(&str, &str)] = &[
        (
            "订单日期 20261117，金额 100000 元，联系电话 13812345678",
            "您的账单",
        ),
        ("会议改到 2026-11-17 14:30，地点 302 会议室", "会议通知"),
        ("快递单号 202611170001 已签收，邮编 100080", "物流更新"),
        (
            "Invoice #A1B2C3D4 total $123456 due 2026/12/01",
            "Your invoice",
        ),
        (
            "Download https://example.com/files/abc123456def",
            "Shared file",
        ),
        ("Order 4f3a9c1e2b details", "Thanks"),
        ("Let's grab hotpot at 1830 tonight", "dinner"),
    ];
    for (body, subject) in cases {
        assert_eq!(
            extract_smart_otp(body, subject),
            None,
            "body={body:?} subject={subject:?}"
        );
    }
}

#[test]
fn smart_otp_does_not_carve_digits_out_of_longer_runs() {
    // 有关键词，但唯一的数字串都是更长的号码/日期——不能从中间截 6 位。
    assert_eq!(extract_smart_otp("验证码已发送到 13800138000", ""), None);
    assert_eq!(extract_smart_otp("验证码订单 202611170001", ""), None);
    assert_eq!(extract_smart_otp("verification for 20261117", ""), None);
    assert_eq!(
        extract_smart_otp("verify token deadbeef123456cafe", ""),
        None
    );
    assert_eq!(extract_smart_otp("验证码金额 ¥123456", ""), None);
}

#[test]
fn raw_extract_code_contract_is_unchanged() {
    assert_eq!(
        extract_code(r"\d{6}", "正文码 111111", "主题 222222").as_deref(),
        Some("111111")
    );
    // 裸正则契约仍会从日期里截出 6 位——智能识别只作用于 OtpPlugin 的默认配置。
    assert_eq!(
        extract_code(r"\d{6}", "订单日期 20261117", "").as_deref(),
        Some("202611")
    );
}

#[test]
fn smart_otp_matches_shared_cross_language_fixture_vectors() {
    #[derive(serde::Deserialize)]
    struct OtpVector {
        name: String,
        subject: String,
        body: String,
        expected: Option<String>,
    }

    let raw = include_str!("fixtures/otp_vectors.json");
    let vectors: Vec<OtpVector> = serde_json::from_str(raw).expect("valid otp_vectors.json");
    assert!(vectors.len() >= 25, "shared OTP vectors must be non-trivial");
    for v in &vectors {
        assert_eq!(
            extract_smart_otp(&v.body, &v.subject),
            v.expected,
            "shared OTP vector {:?} failed (subject={:?}, body={:?})",
            v.name,
            v.subject,
            v.body
        );
    }
}

// —— 2. rules_state.toml ——

fn secure_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

fn full_patch() -> RulesPatch {
    serde_json::from_value(serde_json::json!({
        "show_sensitive": false,
        "silent_keywords": ["广告", "newsletter"],
        "quiet_hours": [{ "start": "22:00", "end": "07:00" }],
        "timezone_offset_minutes": 480,
        "notify_new_mail": true,
        "muted_accounts": ["work"],
    }))
    .unwrap()
}

#[test]
fn ephemeral_settings_are_not_persisted() {
    let settings = RulesSettings::ephemeral(RulesConfig::default());
    assert!(!settings.is_persisted());
    assert!(settings.state_path().is_none());
    let next = settings.patch(&full_patch()).unwrap();
    assert!(!next.show_sensitive);
    assert!(!settings.is_persisted());
}

#[test]
fn persisted_rules_survive_restart_with_private_atomic_file() {
    let dir = secure_dir();
    let path = dir.path().join(RULES_STATE_FILE);
    let base = RulesConfig {
        persist: true,
        ..RulesConfig::default()
    };

    let settings = RulesSettings::open_persistent(base.clone(), path.clone());
    assert!(settings.is_persisted());
    assert!(!path.exists(), "首次打开不制造状态文件");
    let saved = settings.patch(&full_patch()).unwrap();
    assert!(path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    }
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "临时文件已清理");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("format_version = 1"), "{text}");
    assert!(!text.contains("persist"), "persist 只来自 TOML: {text}");

    // 模拟重启：同一基线、同一路径。
    let reopened = RulesSettings::open_persistent(base.clone(), path.clone());
    assert!(reopened.is_persisted());
    let snapshot = reopened.snapshot();
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        serde_json::to_value(&saved).unwrap()
    );
    assert!(!snapshot.show_sensitive);
    assert_eq!(snapshot.silent_keywords, vec!["广告", "newsletter"]);
    assert_eq!(snapshot.quiet_hours.len(), 1);
    assert_eq!(snapshot.timezone_offset_minutes, 480);
    assert!(snapshot.notify_new_mail);
    assert_eq!(snapshot.muted_accounts, vec!["work"]);
    assert!(snapshot.persist);

    // 侧车只存用户改过的字段：没改过的字段继续跟随 TOML 基线。
    let dir2 = secure_dir();
    let path2 = dir2.path().join(RULES_STATE_FILE);
    let partial = RulesSettings::open_persistent(RulesConfig::default(), path2.clone());
    partial
        .patch(&RulesPatch {
            show_sensitive: Some(false),
            ..RulesPatch::default()
        })
        .unwrap();
    let new_base = RulesConfig {
        silent_keywords: vec!["来自新版 TOML".into()],
        ..RulesConfig::default()
    };
    let after_upgrade = RulesSettings::open_persistent(new_base, path2);
    assert!(!after_upgrade.snapshot().show_sensitive);
    assert_eq!(
        after_upgrade.snapshot().silent_keywords,
        vec!["来自新版 TOML"]
    );
}

#[test]
fn invalid_patch_is_rejected_and_not_written() {
    let dir = secure_dir();
    let path = dir.path().join(RULES_STATE_FILE);
    let settings = RulesSettings::open_persistent(RulesConfig::default(), path.clone());
    for muted in [
        vec!["".to_string()],
        vec!["a\u{0007}b".to_string()],
        vec!["x".repeat(129)],
        (0..65).map(|i| format!("acc{i}")).collect(),
    ] {
        let error = settings
            .try_patch(&RulesPatch {
                muted_accounts: Some(muted),
                ..RulesPatch::default()
            })
            .unwrap_err();
        assert!(matches!(error, RulesPatchError::Invalid(_)), "{error:?}");
    }
    assert!(!path.exists(), "非法 patch 不落盘");
    assert!(settings.is_persisted());
}

fn assert_fail_closed(path: &Path, original: &[u8]) {
    let base = RulesConfig {
        silent_keywords: vec!["基线".into()],
        ..RulesConfig::default()
    };
    let settings = RulesSettings::open_persistent(base, path.to_path_buf());
    assert!(!settings.is_persisted());
    assert!(settings.persistence_error().is_some());
    assert_eq!(
        settings.snapshot().silent_keywords,
        vec!["基线"],
        "回退 TOML 基线"
    );
    let error = settings
        .try_patch(&RulesPatch {
            show_sensitive: Some(false),
            ..RulesPatch::default()
        })
        .unwrap_err();
    assert!(
        matches!(error, RulesPatchError::Unavailable(_)),
        "{error:?}"
    );
    assert!(settings.patch(&RulesPatch::default()).is_err());
    assert!(settings.snapshot().show_sensitive, "停摆时内存不变");
    assert_eq!(
        std::fs::read(path).unwrap(),
        original,
        "停摆时绝不覆盖原文件"
    );
}

fn write_private(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn future_version_and_unknown_fields_fail_closed() {
    let dir = secure_dir();
    let path = dir.path().join(RULES_STATE_FILE);

    let future = "format_version = 2\n[rules]\nshow_sensitive = false\nnew_thing = 1\n";
    write_private(&path, future);
    assert_fail_closed(&path, future.as_bytes());

    let unknown = "format_version = 1\n[rules]\nshow_sensitive = false\nmystery = true\n";
    write_private(&path, unknown);
    assert_fail_closed(&path, unknown.as_bytes());

    let invalid =
        "format_version = 1\n[rules]\nquiet_hours = [{ start = \"noon\", end = \"01:00\" }]\n";
    write_private(&path, invalid);
    assert_fail_closed(&path, invalid.as_bytes());

    let missing_version = "[rules]\nshow_sensitive = false\n";
    write_private(&path, missing_version);
    assert_fail_closed(&path, missing_version.as_bytes());

    let huge = format!("format_version = 1\n# {}\n", "x".repeat(64 * 1024 + 1));
    write_private(&path, &huge);
    assert_fail_closed(&path, huge.as_bytes());
}

#[cfg(unix)]
#[test]
fn wide_permissions_and_symlinks_fail_closed() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = secure_dir();
    let path = dir.path().join(RULES_STATE_FILE);
    let body = "format_version = 1\n[rules]\nshow_sensitive = false\n";
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_fail_closed(&path, body.as_bytes());
    // 文件权限没有被「修好」——停摆不改动任何东西。
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
    }

    let dir = secure_dir();
    let target = dir.path().join("elsewhere.toml");
    write_private(&target, body);
    let link = dir.path().join(RULES_STATE_FILE);
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_fail_closed(&link, body.as_bytes());
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn rules_plugin_uses_state_path_service_only_when_persist_enabled() {
    let dir = secure_dir();
    let path = dir.path().join(RULES_STATE_FILE);
    write_private(
        &path,
        "format_version = 1\n[rules]\nsilent_keywords = [\"已保存\"]\n",
    );

    // persist = true + RulesStatePath → 持久模式，加载侧车。
    let ctx = Context::new();
    ctx.provide::<RulesStatePath>(Arc::new(RulesStatePath(path.clone())));
    let config = RulesConfig {
        persist: true,
        ..RulesConfig::default()
    };
    ctx.provide::<RulesConfig>(Arc::new(config.clone()));
    ctx.plugin(Arc::new(RulesPlugin::new(&config)));
    let settings = ctx.get::<RulesSettings>().unwrap();
    assert!(settings.is_persisted());
    assert_eq!(settings.state_path(), Some(path.as_path()));
    assert_eq!(settings.snapshot().silent_keywords, vec!["已保存"]);
    // `persist` 不进 API JSON；关闭的新字段也不出现（旧客户端逐字节兼容）。
    let json = serde_json::to_value(RulesConfig::default()).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "show_sensitive": true,
            "silent_keywords": [],
            "quiet_hours": [],
            "timezone_offset_minutes": 0,
        })
    );

    // persist = false → 即使有路径也不持久。
    let ctx = Context::new();
    ctx.provide::<RulesStatePath>(Arc::new(RulesStatePath(path.clone())));
    let config = RulesConfig::default();
    ctx.provide::<RulesConfig>(Arc::new(config.clone()));
    ctx.plugin(Arc::new(RulesPlugin::new(&config)));
    let settings = ctx.get::<RulesSettings>().unwrap();
    assert!(!settings.is_persisted());
    assert!(settings.snapshot().silent_keywords.is_empty());

    // persist = true 但没有路径 → 退回非持久。
    let ctx = Context::new();
    let config = RulesConfig {
        persist: true,
        ..RulesConfig::default()
    };
    ctx.provide::<RulesConfig>(Arc::new(config.clone()));
    ctx.plugin(Arc::new(RulesPlugin::new(&config)));
    assert!(!ctx.get::<RulesSettings>().unwrap().is_persisted());

    // 显式 with_state_path。
    let ctx = Context::new();
    let config = RulesConfig {
        persist: true,
        ..RulesConfig::default()
    };
    ctx.provide::<RulesConfig>(Arc::new(config.clone()));
    ctx.plugin(Arc::new(RulesPlugin::with_state_path(&config, path)));
    assert!(ctx.get::<RulesSettings>().unwrap().is_persisted());
}

#[test]
fn toml_plugin_config_accepts_new_fields() {
    let config: RulesConfig = toml::from_str(
        "show_sensitive = true\nnotify_new_mail = true\nmuted_accounts = [\"work\"]\npersist = true\n",
    )
    .unwrap();
    assert!(config.notify_new_mail && config.persist);
    assert_eq!(config.muted_accounts, vec!["work"]);
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(json["notify_new_mail"], true);
    assert_eq!(json["muted_accounts"], serde_json::json!(["work"]));
    assert!(json.get("persist").is_none());
}

// —— 3. 普适新邮件通知 ——

fn notify_ctx(rules: RulesConfig, with_otp: bool) -> Context {
    let ctx = Context::new();
    ctx.provide::<Store>(Arc::new(Store::open("").expect("内存库")));
    ctx.provide::<Notifier>(Arc::new(Notifier::new("dev-log").unwrap()));
    ctx.provide::<NotifyConfig>(Arc::new(NotifyConfig::default()));
    ctx.provide::<RulesConfig>(Arc::new(rules.clone()));
    if with_otp {
        ctx.provide::<OtpConfig>(Arc::new(OtpConfig::default()));
        ctx.plugin(Arc::new(OtpPlugin));
    }
    ctx.plugin(Arc::new(RulesPlugin::new(&rules)));
    ctx
}

fn enabled() -> RulesConfig {
    RulesConfig {
        notify_new_mail: true,
        ..RulesConfig::default()
    }
}

fn mail(account: &str, uid: &str) -> MailMessage {
    MailMessage {
        account: account.into(),
        uid: uid.into(),
        sender: "\"张三\" <zhang@example.com>".into(),
        subject: "周报：第 46 周进展".into(),
        body: "本周完成了  通知规则\n持久化 与 合并限流。".into(),
        folder: "INBOX".into(),
        ..Default::default()
    }
}

/// 按提交顺序（`Notifier::sent` 是新的在前）。
fn sent(ctx: &Context) -> Vec<maild::notify::Notification> {
    let mut sent = ctx.get::<Notifier>().unwrap().sent();
    sent.sort_by_key(|notification| notification.id);
    sent
}

#[test]
fn unread_inbox_mail_notifies_with_message_target() {
    let ctx = notify_ctx(enabled(), true);
    ctx.emit::<MailFiltered>(mail("personal", "41"));
    let sent = sent(&ctx);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].title, "张三");
    assert!(sent[0].body.contains("周报：第 46 周进展"));
    assert!(sent[0]
        .body
        .contains("本周完成了 通知规则 持久化 与 合并限流。"));
    assert_eq!(
        sent[0].target,
        Some(OpenIntent::Message {
            account: "personal".into(),
            uid: "41".into(),
            folder: Some("INBOX".into()),
        })
    );
    // 同一封重复投递不重复通知。
    ctx.emit::<MailFiltered>(mail("personal", "41"));
    assert_eq!(self::sent(&ctx).len(), 1);
}

#[test]
fn disabled_seen_and_muted_mail_do_not_notify() {
    let ctx = notify_ctx(RulesConfig::default(), true);
    ctx.emit::<MailFiltered>(mail("personal", "1"));
    assert!(sent(&ctx).is_empty(), "notify_new_mail 缺省关闭");

    let ctx = notify_ctx(
        RulesConfig {
            muted_accounts: vec!["work".into()],
            ..enabled()
        },
        true,
    );
    ctx.emit::<MailFiltered>(MailMessage {
        seen: true,
        ..mail("personal", "2")
    });
    ctx.emit::<MailFiltered>(mail("work", "3"));
    // 积压信（Date 早于 24 小时）。
    ctx.emit::<MailFiltered>(MailMessage {
        date_at: Some(1_000_000_000),
        ..mail("personal", "4")
    });
    assert!(sent(&ctx).is_empty(), "{:?}", sent(&ctx));

    // 运行时 patch 立即生效：解除静音。
    ctx.get::<RulesSettings>()
        .unwrap()
        .patch(&RulesPatch {
            muted_accounts: Some(vec![]),
            ..RulesPatch::default()
        })
        .unwrap();
    ctx.emit::<MailFiltered>(mail("work", "5"));
    assert_eq!(sent(&ctx).len(), 1);
}

#[test]
fn otp_mail_gets_only_the_otp_notification() {
    // 两种挂载顺序都只有一条（rules 先 / otp 先）。
    for otp_first in [true, false] {
        let rules = enabled();
        let ctx = Context::new();
        ctx.provide::<Store>(Arc::new(Store::open("").unwrap()));
        ctx.provide::<Notifier>(Arc::new(Notifier::new("dev-log").unwrap()));
        ctx.provide::<NotifyConfig>(Arc::new(NotifyConfig::default()));
        ctx.provide::<RulesConfig>(Arc::new(rules.clone()));
        ctx.provide::<OtpConfig>(Arc::new(OtpConfig::default()));
        if otp_first {
            ctx.plugin(Arc::new(OtpPlugin));
            ctx.plugin(Arc::new(RulesPlugin::new(&rules)));
        } else {
            ctx.plugin(Arc::new(RulesPlugin::new(&rules)));
            ctx.plugin(Arc::new(OtpPlugin));
        }
        ctx.emit::<MailFiltered>(MailMessage {
            subject: "登录验证码".into(),
            body: "你的验证码是 246810，5 分钟内有效。".into(),
            ..mail("personal", "9")
        });
        let sent = sent(&ctx);
        assert_eq!(sent.len(), 1, "otp_first={otp_first}: {sent:?}");
        assert_eq!(sent[0].title, "验证码");
        assert!(sent[0].body.contains("246810"));
    }
}

#[test]
fn ordinary_bill_is_not_an_otp_but_is_a_new_mail() {
    let ctx = notify_ctx(enabled(), true);
    ctx.emit::<MailFiltered>(MailMessage {
        subject: "您的账单".into(),
        body: "订单日期 20261117，金额 100000 元，联系电话 13812345678".into(),
        ..mail("personal", "10")
    });
    let sent = sent(&ctx);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_ne!(sent[0].title, "验证码", "账单不得误报为验证码");
    assert_eq!(sent[0].title, "张三", "按普通新邮件通知");
    assert!(ctx
        .get::<Store>()
        .unwrap()
        .is_notified("personal", "10")
        .map(|notified| !notified)
        .unwrap());
}

#[test]
fn masked_preview_hides_subject_and_snippet() {
    let ctx = notify_ctx(
        RulesConfig {
            show_sensitive: false,
            ..enabled()
        },
        true,
    );
    ctx.emit::<MailFiltered>(MailMessage {
        snippet: "机密摘要".into(),
        ..mail("personal", "11")
    });
    let sent = sent(&ctx);
    assert_eq!(sent.len(), 1, "非敏感新邮件通知不被 show_sensitive 静默");
    assert_eq!(sent[0].title, "新邮件");
    assert_eq!(sent[0].body, "账户 personal 收到一封新邮件");
    for leaked in ["周报", "机密摘要", "张三", "zhang@example.com"] {
        assert!(!sent[0].title.contains(leaked) && !sent[0].body.contains(leaked));
    }
}

#[test]
fn silent_keywords_and_quiet_hours_apply_to_new_mail() {
    let ctx = notify_ctx(
        RulesConfig {
            silent_keywords: vec!["周报".into()],
            ..enabled()
        },
        false,
    );
    ctx.emit::<MailFiltered>(mail("personal", "12"));
    assert!(sent(&ctx).is_empty());

    let ctx = notify_ctx(
        RulesConfig {
            quiet_hours: vec![maild::rules::QuietHoursConfig {
                start: "00:00".into(),
                end: "24:00".into(),
            }],
            ..enabled()
        },
        false,
    );
    ctx.emit::<MailFiltered>(mail("personal", "13"));
    assert!(sent(&ctx).is_empty());
}

/// ADR-0048 决策 6：通知在**成文时**按 daemon 的语言偏好协商；切换偏好后下一条即跟随，
/// 用户自定义的 OTP 标题原样保留。
#[test]
fn notifications_follow_locale_preference_at_compose_time() {
    use maild::locale_preference::{LocaleMode, LocalePreference, LocalePreferenceRequest};

    let dir = tempfile::tempdir().unwrap();
    let preference = Arc::new(LocalePreference::open_with_system(
        dir.path().join("locale_preference.toml"),
        || vec!["en-US".to_string()],
    ));
    // show_sensitive 缺省打开：验证码不被静默；新邮件用无主题信验证占位文案。
    let ctx = notify_ctx(enabled(), true);
    ctx.provide::<LocalePreference>(preference.clone());
    let untitled = |uid: &str| MailMessage {
        subject: " ".into(),
        ..mail("personal", uid)
    };

    // system 模式 → 系统首选 en-US → 英文。
    ctx.emit::<MailFiltered>(untitled("l1"));
    let otp_mail = |uid: &str| MailMessage {
        subject: "登录验证码".into(),
        body: "你的验证码是 246810，5 分钟内有效。".into(),
        ..mail("personal", uid)
    };
    ctx.emit::<MailFiltered>(otp_mail("l2"));
    let english = sent(&ctx);
    assert_eq!(english.len(), 2, "{english:?}");
    assert_eq!(english[0].title, "张三", "发件人是用户数据，原样显示");
    assert!(
        english[0].body.starts_with("(No Subject) "),
        "{:?}",
        english[0].body
    );
    assert_eq!(english[1].title, "Verification Code");
    assert_eq!(
        english[1].body,
        "246810 (personal · from \"张三\" <zhang@example.com>)"
    );

    // 显式切到简体中文：不重启、不重装插件，下一条通知即为中文。
    preference
        .set(&LocalePreferenceRequest {
            mode: LocaleMode::Explicit,
            languages: Some(vec!["zh-Hans-CN".into(), "en".into()]),
        })
        .unwrap();
    ctx.emit::<MailFiltered>(untitled("l3"));
    ctx.emit::<MailFiltered>(otp_mail("l4"));
    let all = sent(&ctx);
    assert_eq!(all.len(), 4, "{all:?}");
    assert_eq!(all[2].title, "张三");
    assert!(all[2].body.starts_with("（无主题） "), "{:?}", all[2].body);
    assert_eq!(all[3].title, "验证码");
    assert_eq!(
        all[3].body,
        "246810（personal · 来自 \"张三\" <zhang@example.com>）"
    );
}

#[test]
fn custom_otp_title_survives_locale_switch() {
    use maild::locale_preference::LocalePreference;

    let dir = tempfile::tempdir().unwrap();
    let ctx = Context::new();
    ctx.provide::<Store>(Arc::new(Store::open("").unwrap()));
    ctx.provide::<Notifier>(Arc::new(Notifier::new("dev-log").unwrap()));
    ctx.provide::<NotifyConfig>(Arc::new(NotifyConfig::default()));
    ctx.provide::<LocalePreference>(Arc::new(LocalePreference::open_with_system(
        dir.path().join("locale_preference.toml"),
        || vec!["en".to_string()],
    )));
    ctx.provide::<OtpConfig>(Arc::new(
        toml::from_str::<OtpConfig>("title = \"登录码\"\n").unwrap(),
    ));
    ctx.plugin(Arc::new(OtpPlugin));
    ctx.emit::<MailFiltered>(MailMessage {
        body: "Your verification code is 135790".into(),
        ..mail("personal", "c1")
    });
    let sent = sent(&ctx);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].title, "登录码", "用户写的标题原样保留");
    assert!(sent[0].body.starts_with("135790 (personal · from "));
}

#[test]
fn burst_of_ten_mails_is_merged() {
    let ctx = notify_ctx(enabled(), true);
    for uid in 0..10 {
        ctx.emit::<MailFiltered>(mail("personal", &format!("b{uid}")));
    }
    ctx.emit::<MailFiltered>(mail("other", "o1"));
    let sent = sent(&ctx);
    assert_eq!(
        sent.len(),
        5,
        "3 封逐封 + 1 条合并 + 另一账户 1 封: {sent:?}"
    );
    for single in &sent[..3] {
        assert!(
            matches!(single.target, Some(OpenIntent::Message { .. })),
            "{sent:#?}"
        );
    }
    assert_eq!(sent[3].body, "账户 personal 收到多封新邮件");
    assert_eq!(
        sent[3].target,
        Some(OpenIntent::Inbox {
            account: Some("personal".into())
        })
    );
    assert!(matches!(
        &sent[4].target,
        Some(OpenIntent::Message { account, .. }) if account == "other"
    ));
    let metrics = ctx.get::<Notifier>().unwrap().metrics();
    assert!(
        metrics.submitted + metrics.dropped + metrics.failed <= 5,
        "限流后进入 Notifier 的通知不超过 5 条: {metrics:?}"
    );
}
