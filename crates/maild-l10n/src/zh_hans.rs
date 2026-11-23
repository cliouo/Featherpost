//! 简体中文（基础语言）。文案与迁移前代码中的字面量逐字节一致。

use crate::{AttributionDate, Launcher, Menu, Messages, Notifications, Quote};

pub const ZH_HANS: Messages = Messages {
    notifications: Notifications {
        otp_title: "验证码",
        otp_body,
        new_mail_title: "新邮件",
        new_mail_burst_body,
        new_mail_masked_body,
        no_subject: "（无主题）",
    },
    quote: Quote {
        reply_attribution,
        forward_header,
        attribution_date,
    },
    menu: Menu {
        open_inbox: "打开收件箱",
        sync_now: "立即同步邮件",
        compose: "写邮件…",
        settings: "设置…",
        background_and_notifications: "后台与通知…",
        repair_and_background: "修复与后台管理…",
        quit,
    },
    launcher: Launcher {
        window_title,
        status_summary,
        status_online: "已连接",
        status_offline: "未连接",
        mailbox_ready: "已配置阅读入口",
        mailbox_needs_check: "需要检查",
        note_closing_keeps_receiving: "关闭此窗口不会停止收信。",
        note_open_from_full_app,

        button_open_mailbox: "打开邮箱",
        button_repair_options: "修复选项…",
        button_close: "关闭",
        button_restore_builtin: "恢复内置前端",
        button_background: "后台管理…",
        button_back: "返回",
        button_stop_background: "停止后台",
        button_start_once: "启动一次",
        button_login_item: "登录项…",
        button_launch_at_login: "开机启动…",
        button_system_login_items: "系统登录项…",
        button_enable: "启用",
        button_disable: "停用",
        button_cancel: "取消",
        button_continue: "继续",
        button_ok: "好",

        repair_options_prompt: "邮件窗口打不开时，可从安装包恢复内置前端。\n\n邮件、账户与凭据保持不变；现有前端包保留备份。",
        confirm_restore_title: "恢复内置前端？",
        confirm_restore_body: "将恢复安装包自带的邮件前端，并设为默认。现有前端包会保留备份。\n\n继续前请关闭邮件阅读窗口。",
        confirm_stop_title: "停止收信后台？",
        confirm_stop_body: "收信、规则和系统通知将暂停。可以从此窗口再次启动后台。",
        login_prompt: "开机启动由系统管理。注册或撤销后，可在系统登录项设置中检查。",
        login_choice_prompt: "选择开机启动行为。此处不会改变邮件或账户。",

        login_isolated: "隔离目录：不更改系统登录项",
        login_registered: "已注册",
        login_requires_approval: "等待系统批准",
        login_not_registered: "未注册",
        login_unavailable: "不可用",
        login_unsupported_platform: "当前平台暂不提供登录项管理",
        login_manage_from_app,

        result_mailbox_requested: "已请求打开邮箱。确认邮件窗口打开后，可以关闭修复窗口。",
        result_mailbox_queued: "打开请求已送入队列，请切回正在运行的邮件窗口。",
        result_repaired_online: "内置前端已恢复，并已设为默认。原包已保留备份。",
        result_repaired_offline: "内置前端已恢复，并已设为默认。启动后台后即可打开邮箱。",
        result_background_connected: "后台已连接",
        result_start_requested: "已请求启动一次，返回主窗口可刷新连接状态",
        result_stop_accepted: "后台已接受停止请求，正在保存状态并退出",
        result_login_register_requested: "已提交开机启动请求。如系统要求批准，请打开系统登录项设置。",
        result_login_unregistered: "已撤销开机启动注册，请返回检查后台状态",
        result_login_settings_requested: "已请求打开系统登录项设置",

        error_unsupported_by_agent: "后台版本不支持此操作，请先退出旧后台并使用完整的新版本应用",
        error_connection_setup: "无法建立本机后台连接",
        error_app_incomplete: "应用不完整，无法安全管理后台",
        error_local_connection_unavailable: "本机连接不可用，请从完整应用包打开",
        error_operation_unconfirmed: "操作尚未确认，请刷新状态；不要重复执行修复",
        error_repair_incomplete: "修复未完整确认。请检查安装包、目录占用与磁盘空间；如磁盘已恢复，请重启后台后检查。不要自动重试。",
        error_agent_unconfirmed: "后台未确认操作，请刷新状态并检查版本；修复或停止请求可能仍在执行，不要自动重试",
        error_custom_connection: "此入口使用自定义连接，不能管理平台后台",
        error_open_from_full_app: "请从完整应用包打开修复与后台管理",
        error_mailbox_unavailable: "邮件前端尚不可用，请先恢复内置前端或启动后台",
        error_mailbox_cannot_open: "当前前端无法自动打开。可切回已有邮件窗口，或从修复选项恢复内置前端。",
        error_repair_unconfirmed: "后台未确认内置前端修复，请刷新状态",
        error_repair_offline_failed: "未能完成离线恢复。请确认所有后台已停止、安装包完整，并检查磁盘空间；不要自动重试。",
        error_platform_dirs_unavailable: "平台目录不可用",
        error_background_busy: "后台仍被占用或正在启动，请稍后刷新",
        error_background_config_unavailable: "本机后台配置不可用，请检查完整应用的安装",
        error_start_failed: "无法启动内嵌后台",
        error_stop_unconfirmed: "后台未确认停止，请刷新状态",
        error_login_isolated_register: "当前使用隔离用户目录，不能注册系统登录项",
        error_login_register_failed: "系统未完成登录项注册，请检查应用签名并在系统设置中批准",
        error_login_isolated_unregister: "当前使用隔离用户目录，不能撤销系统登录项",
        error_login_unregister_failed: "系统未完成登录项撤销，请在系统设置中检查",
        error_agent_path_invalid: "内嵌后台路径无效",
        error_agent_launch_failed: "系统未能启动内嵌后台，请检查应用签名与完整性",
        error_agent_launch_unsupported: "当前平台暂不提供后台应用启动",
        error_open_mail_window_failed: "无法打开邮件窗口",
        error_in_place_unsupported: "当前平台不支持原位打开邮件窗口",
    },
    ctl: crate::ctl::ZH_HANS,
};

const WEEKDAYS: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

fn otp_body(code: &str, account: Option<&str>, from: &str) -> String {
    match account {
        Some(account) => format!("{code}（{account} · 来自 {from}）"),
        None => format!("{code}（来自 {from}）"),
    }
}

fn new_mail_burst_body(account: Option<&str>) -> String {
    match account {
        Some(account) => format!("账户 {account} 收到多封新邮件"),
        None => "收到多封新邮件".into(),
    }
}

fn new_mail_masked_body(account: Option<&str>) -> String {
    match account {
        Some(account) => format!("账户 {account} 收到一封新邮件"),
        None => "收到一封新邮件".into(),
    }
}

fn reply_attribution(date: &str, sender: &str) -> String {
    format!("在 {date}，{sender} 写道：")
}

fn forward_header(sender: &str, date: &str, subject: &str, recipient: &str) -> String {
    format!(
        "---------- 转发的邮件 ----------\n\
         发件人: {sender}\n日期: {date}\n主题: {subject}\n收件人: {recipient}"
    )
}

fn attribution_date(date: &AttributionDate) -> String {
    let (sign, offset_hours, offset_minutes) = date.utc_offset();
    let weekday = WEEKDAYS[date.weekday_index()];
    format!(
        "{}年{}月{}日 {weekday} {:02}:{:02}（UTC{sign}{offset_hours:02}:{offset_minutes:02}）",
        date.year, date.month, date.day, date.hour, date.minute,
    )
}

fn quit(product: &str) -> String {
    format!("退出 {product}")
}

fn window_title(product: &str) -> String {
    format!("{product} 修复与后台管理")
}

fn status_summary(background: &str, mailbox: &str, login: &str, note: &str) -> String {
    format!("后台：{background}\n邮件前端：{mailbox}\n开机启动：{login}\n\n{note}")
}

fn note_open_from_full_app(app_bundle: &str) -> String {
    format!("请从完整 {app_bundle} 打开，以管理后台和恢复邮件前端。")
}

fn login_manage_from_app(app_bundle: &str) -> String {
    format!("请从 {app_bundle} 管理")
}
