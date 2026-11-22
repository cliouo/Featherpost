use super::*;

#[test]
fn negotiation_table() {
    let cases: &[(&[&str], Locale)] = &[
        (&["zh-Hans-CN"], Locale::ZhHans),
        (&["zh-Hans"], Locale::ZhHans),
        (&["zh-CN"], Locale::ZhHans),
        (&["zh-SG"], Locale::ZhHans),
        (&["zh"], Locale::ZhHans),
        (&["zh_CN"], Locale::ZhHans),
        (&["ZH-hans-cn"], Locale::ZhHans),
        (&["zh-Hant-TW"], Locale::ZhHans),
        (&["zh-Hant-TW", "en"], Locale::En),
        (&["zh-TW", "en"], Locale::En),
        (&["zh_HK", "en"], Locale::En),
        (&["zh-MO", "en-GB"], Locale::En),
        (&["zh-Hant", "zh-CN"], Locale::ZhHans),
        (&["en-US"], Locale::En),
        (&["EN"], Locale::En),
        (&["en_GB"], Locale::En),
        (&["en-US", "zh-Hans"], Locale::En),
        (&["zh-Hans-CN", "en-US"], Locale::ZhHans),
        (&["ja", "en"], Locale::En),
        (&["ja-JP", "fr", "zh-Hans"], Locale::ZhHans),
        (&[], Locale::ZhHans),
        (&["ja"], Locale::ZhHans),
        (&["fr-FR", "de"], Locale::ZhHans),
        (&["", "en"], Locale::En),
        (&["eng", "english", "en"], Locale::En),
        (&["zh-Latn", "en"], Locale::En),
        (&["zh-cmn-Hans-CN"], Locale::ZhHans),
        (&["zh-419"], Locale::ZhHans),
    ];
    for (effective, expected) in cases {
        assert_eq!(negotiate(effective), *expected, "effective = {effective:?}");
    }
    // `zh-Hant-TW` 单独出现时没有可匹配项，回落基础语言而不是“匹配”繁体。
    assert_eq!(match_tag("zh-Hant-TW"), None);
    assert_eq!(match_tag("zh-TW"), None);
}

#[test]
fn negotiate_accepts_owned_strings() {
    let effective = vec![String::from("ja-JP"), String::from("en-US")];
    assert_eq!(negotiate(&effective), Locale::En);
}

#[test]
fn locale_tags_and_catalogs() {
    assert_eq!(Locale::default(), Locale::ZhHans);
    assert_eq!(Locale::ZhHans.tag(), "zh-Hans");
    assert_eq!(Locale::En.tag(), "en");
    for locale in Locale::ALL {
        // 规范标签协商回自己。
        assert_eq!(negotiate(&[locale.tag()]), locale);
    }
    assert_eq!(Locale::ZhHans.messages().menu.open_inbox, "打开收件箱");
    assert_eq!(Locale::En.messages().menu.open_inbox, "Open Inbox");
}

// —— 带参数消息的快照：中文与迁移前代码的拼法逐字节一致 ——

#[test]
fn notifications_snapshot() {
    let zh = &ZH_HANS.notifications;
    let (code, account, from) = ("123456", "work", "Alice <a@example.test>");
    // 迁移前：otp.rs 的 `format!("{}（{} · 来自 {}）", hit.code, hit.account, hit.from)`。
    assert_eq!(
        (zh.otp_body)(code, account, from),
        format!("{}（{} · 来自 {}）", code, account, from)
    );
    assert_eq!(
        (zh.otp_body)(code, account, from),
        "123456（work · 来自 Alice <a@example.test>）"
    );
    assert_eq!((zh.new_mail_burst_body)("work"), "账户 work 收到多封新邮件");
    assert_eq!(
        (zh.new_mail_masked_body)("work"),
        "账户 work 收到一封新邮件"
    );

    let en = &EN.notifications;
    assert_eq!(
        (en.otp_body)(code, account, from),
        "123456 (work · from Alice <a@example.test>)"
    );
    assert_eq!(
        (en.new_mail_burst_body)("work"),
        "Several new messages arrived in work"
    );
    assert_eq!(
        (en.new_mail_masked_body)("work"),
        "A new message arrived in work"
    );
}

#[test]
fn quote_snapshot() {
    let (sender, date, subject, recipient, body) = (
        "Alice <a@example.test>",
        "2026年11月18日 周三 19:35（UTC+08:00）",
        "周会",
        "bob@example.test",
        "> hi",
    );
    let zh = &ZH_HANS.quote;
    // 迁移前：compose.rs 的 `quote_reply` / `quote_forward` 整段拼法。
    assert_eq!(
        format!("\n\n{}\n{body}", (zh.reply_attribution)(date, sender)),
        format!("\n\n在 {date}，{sender} 写道：\n{body}")
    );
    assert_eq!(
        format!(
            "\n\n{}\n\n{body}",
            (zh.forward_header)(sender, date, subject, recipient)
        ),
        format!(
            "\n\n---------- 转发的邮件 ----------\n\
             发件人: {sender}\n日期: {date}\n主题: {subject}\n收件人: {recipient}\n\n{body}"
        )
    );

    let en = &EN.quote;
    assert_eq!(
        (en.reply_attribution)("Wed, Nov 18, 2026 at 19:35 (UTC+08:00)", "Alice"),
        "On Wed, Nov 18, 2026 at 19:35 (UTC+08:00), Alice wrote:"
    );
    assert_eq!(
        (en.forward_header)("Alice", "Wed", "Weekly", "bob@example.test"),
        "---------- Forwarded message ----------\n\
         From: Alice\nDate: Wed\nSubject: Weekly\nTo: bob@example.test"
    );
}

fn date(
    (year, month, day, weekday): (i32, u8, u8, u8),
    (hour, minute, utc_offset_minutes): (u8, u8, i32),
) -> AttributionDate {
    AttributionDate {
        year,
        month,
        day,
        weekday,
        hour,
        minute,
        utc_offset_minutes,
    }
}

#[test]
fn attribution_date_snapshot() {
    // 与 compose.rs `format_attribution_parts` 的既有测试同一组输入与期望。
    let cases = [
        (
            date((2026, 11, 18, 3), (19, 35, 480)),
            "2026年11月18日 周三 19:35（UTC+08:00）",
            "Wed, Nov 18, 2026 at 19:35 (UTC+08:00)",
        ),
        (
            date((2026, 11, 18, 0), (7, 5, -360)),
            "2026年11月18日 周日 07:05（UTC-06:00）",
            "Sun, Nov 18, 2026 at 07:05 (UTC-06:00)",
        ),
        (
            date((2026, 3, 1, 6), (0, 0, 330)),
            "2026年3月1日 周六 00:00（UTC+05:30）",
            "Sat, Mar 1, 2026 at 00:00 (UTC+05:30)",
        ),
        (
            date((2026, 1, 2, 5), (12, 30, 0)),
            "2026年1月2日 周五 12:30（UTC+00:00）",
            "Fri, Jan 2, 2026 at 12:30 (UTC+00:00)",
        ),
        // 越界星期按周六、越界月份退回数字：都不 panic。
        (
            date((2026, 13, 2, 9), (12, 30, 0)),
            "2026年13月2日 周六 12:30（UTC+00:00）",
            "Sat, 13 2, 2026 at 12:30 (UTC+00:00)",
        ),
    ];
    for (input, zh, en) in cases {
        assert_eq!((ZH_HANS.quote.attribution_date)(&input), zh);
        assert_eq!((EN.quote.attribution_date)(&input), en);
    }
}

#[test]
fn menu_snapshot() {
    assert_eq!((ZH_HANS.menu.quit)("Featherpost"), "退出 Featherpost");
    assert_eq!((EN.menu.quit)("Featherpost"), "Quit Featherpost");
}

#[test]
fn launcher_snapshot() {
    let zh = &ZH_HANS.launcher;
    assert_eq!(
        (zh.window_title)("Featherpost"),
        "Featherpost 修复与后台管理"
    );
    // 迁移前：ui.rs `status_text` 的模板。
    let (background, mailbox, login, note) = ("已连接", "需要检查", "未注册", "提示");
    assert_eq!(
        (zh.status_summary)(background, mailbox, login, note),
        format!(
            "后台：{}\n邮件前端：{}\n开机启动：{}\n\n{}",
            background, mailbox, login, note
        )
    );
    assert_eq!(
        (zh.note_open_from_full_app)("Featherpost.app"),
        "请从完整 Featherpost.app 打开，以管理后台和恢复邮件前端。"
    );
    assert_eq!(
        (zh.login_manage_from_app)("Featherpost.app"),
        "请从 Featherpost.app 管理"
    );

    let en = &EN.launcher;
    assert_eq!(
        (en.window_title)("Featherpost"),
        "Featherpost Repair & Background"
    );
    assert_eq!(
        (en.status_summary)("Connected", "Needs attention", "Not registered", "Note."),
        "Background: Connected\nMail frontend: Needs attention\nLaunch at login: Not registered\n\nNote."
    );
    assert_eq!(
        (en.note_open_from_full_app)("Featherpost.app"),
        "Open the complete Featherpost.app to manage the background and restore the mail frontend."
    );
    assert_eq!(
        (en.login_manage_from_app)("Featherpost.app"),
        "Manage from Featherpost.app"
    );
}

/// 同一组对话框的按钮必须互不相同：非 macOS 的 `rfd` 按按钮文字反查被点的是哪一个。
#[test]
fn dialog_button_labels_are_distinct() {
    for messages in [&ZH_HANS, &EN] {
        let l = &messages.launcher;
        let groups: [&[&str]; 7] = [
            &[
                l.button_open_mailbox,
                l.button_repair_options,
                l.button_close,
            ],
            &[l.button_restore_builtin, l.button_background, l.button_back],
            &[l.button_stop_background, l.button_login_item, l.button_back],
            &[l.button_start_once, l.button_login_item, l.button_back],
            &[
                l.button_launch_at_login,
                l.button_system_login_items,
                l.button_back,
            ],
            &[l.button_enable, l.button_disable, l.button_cancel],
            &[l.button_continue, l.button_cancel],
        ];
        for group in groups {
            for (index, label) in group.iter().enumerate() {
                assert!(!label.is_empty());
                assert!(!group[index + 1..].contains(label), "duplicate {label:?}");
            }
        }
    }
}
