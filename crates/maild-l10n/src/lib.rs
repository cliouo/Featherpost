//! maild-l10n：内核自己的界面文案目录（ADR-0048 决策 6）。
//!
//! 内核界面的语言 = 用户偏好的 `effective` 有序列表（ADR-0048 决策 2）与本 crate 支持的语言
//! 协商的结果，见 [`negotiate`]。首版支持 `zh-Hans`（基础语言，协商失败时的回落）与 `en`。
//!
//! # 范围
//!
//! 只收**内核自己画出来、给人看的界面文字**：
//!
//! - 系统通知（新邮件、验证码）：调用方在**发出通知时**按当时的偏好协商并成文；
//! - 菜单栏 companion（`maild-menu`）；
//! - 原生启动器与修复对话框（`maild-launcher`，ADR-0039）；
//! - 回复/转发引用块（ADR-0048 决策 7）：它写进发出去的邮件正文，语言由 `prepareReply` 的
//!   `locale` 参数或 `effective` 列表协商，可以与界面语言不同。
//!
//! 不收（ADR-0048 非目标）：
//!
//! - 日志、`eprintln!`、配置/校验报错、协议 `error.message` 等面向开发者的文字；
//! - 用户或服务商数据：账户名、发件人、主题、文件夹显示名、邮件内容；
//! - OTP 识别关键词（`otp.rs` 的「验证码 / 校验码 …」）——那是内容识别规则，不随界面语言切换；
//! - 默认前端的文案（含 Tauri 原生菜单），它们归默认前端自己的翻译方案（决策 8）。
//!
//! 产品名一律是参数，不写死在这里：品牌来自 `maild-brand`，由调用方传入。
//!
//! # 形状
//!
//! 每种语言一个 `const` [`Messages`] 值（[`ZH_HANS`]、[`EN`]），字段即消息；带参数的消息是
//! `fn` 字段，调用写作 `(m.menu.quit)(PRODUCT_NAME)`。新增消息时两种语言都必须填字段，
//! **缺字段即编译失败**，不需要额外的 key 一致性检查。按界面分组：[`Notifications`]、
//! [`Quote`]、[`Menu`]、[`Launcher`]。
//!
//! 本 crate 零依赖，只做协商与成文，不读系统语言、不读偏好文件。

mod en;
mod zh_hans;

pub use en::EN;
pub use zh_hans::ZH_HANS;

/// 内核界面支持的语言。
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Locale {
    /// 简体中文，基础语言；协商不出结果时回落到它（即今天的行为）。
    #[default]
    ZhHans,
    /// 英文。
    En,
}

impl Locale {
    /// 全部支持的语言，基础语言在前。
    pub const ALL: [Locale; 2] = [Locale::ZhHans, Locale::En];

    /// 规范 BCP 47 标签。
    pub const fn tag(self) -> &'static str {
        match self {
            Locale::ZhHans => "zh-Hans",
            Locale::En => "en",
        }
    }

    /// 该语言的文案目录。
    pub const fn messages(self) -> &'static Messages {
        match self {
            Locale::ZhHans => &ZH_HANS,
            Locale::En => &EN,
        }
    }
}

/// 用有序偏好列表协商内核界面语言（ADR-0048 决策 6、8 的协商规则）。
///
/// 按顺序逐项看，第一项能匹配的即为结果：
///
/// - 按语言子标签匹配，大小写不敏感，`-` 与 `_` 都可作分隔符；
/// - `en*` 匹配 [`Locale::En`]；
/// - `zh` 额外比较书写系统：`zh-Hans*` 与无书写系统的 `zh` / `zh-CN` / `zh-SG` 匹配
///   [`Locale::ZhHans`]；`zh-Hant*` 以及无书写系统的 `zh-TW` / `zh-HK` / `zh-MO` 不匹配，
///   继续看下一项；
/// - 全部不匹配（含空列表）回落 [`Locale::ZhHans`]。
pub fn negotiate<S: AsRef<str>>(effective: &[S]) -> Locale {
    effective
        .iter()
        .find_map(|tag| match_tag(tag.as_ref()))
        .unwrap_or_default()
}

/// 单个标签的匹配；`None` 表示看下一项。
fn match_tag(tag: &str) -> Option<Locale> {
    let mut subtags = tag.trim().split(['-', '_']);
    let language = subtags.next()?;
    if language.eq_ignore_ascii_case("en") {
        return Some(Locale::En);
    }
    if !language.eq_ignore_ascii_case("zh") {
        return None;
    }
    for subtag in subtags {
        let alphabetic = subtag.bytes().all(|byte| byte.is_ascii_alphabetic());
        match subtag.len() {
            // 书写系统子标签（4 个字母）：只有 Hans 算简体中文。
            4 if alphabetic => {
                return subtag
                    .eq_ignore_ascii_case("Hans")
                    .then_some(Locale::ZhHans);
            }
            // 地区子标签出现在书写系统之前：已没有书写系统，按地区推断。
            2 if alphabetic => {
                let traditional = ["TW", "HK", "MO"]
                    .iter()
                    .any(|region| subtag.eq_ignore_ascii_case(region));
                return (!traditional).then_some(Locale::ZhHans);
            }
            3 if subtag.bytes().all(|byte| byte.is_ascii_digit()) => {
                return Some(Locale::ZhHans);
            }
            // 扩展语言子标签（如 `zh-cmn-Hans`）等：继续往后找书写系统或地区。
            _ => {}
        }
    }
    Some(Locale::ZhHans)
}

/// 一种语言的全部内核界面文案。
#[derive(Clone, Copy, Debug)]
pub struct Messages {
    pub notifications: Notifications,
    pub quote: Quote,
    pub menu: Menu,
    pub launcher: Launcher,
}

/// 系统通知（`maild` 的 `otp` 与 `rules::mail_notify`）。
///
/// 账户名、发件人、主题与摘要是用户/服务商数据，原样作为参数传入。
#[derive(Clone, Copy, Debug)]
pub struct Notifications {
    /// 验证码通知标题（`OtpConfig::title` 的出厂默认值）。
    pub otp_title: &'static str,
    /// 验证码通知正文。
    pub otp_body: fn(code: &str, account: &str, from: &str) -> String,
    /// 新邮件通知的脱敏标题；合并提示也用它。
    pub new_mail_title: &'static str,
    /// 同一账户短时间内多封新邮件的合并提示正文。
    pub new_mail_burst_body: fn(account: &str) -> String,
    /// 不显示敏感内容时，单封新邮件的正文。
    pub new_mail_masked_body: fn(account: &str) -> String,
    /// 显示敏感内容但邮件没有主题时的占位。
    pub no_subject: &'static str,
}

/// 回复/转发写进邮件正文的引用块（`maild` 的 `compose`）。
///
/// 只给出成文的那几行；前后的空行、`> ` 引用前缀与原文正文由调用方拼接。
#[derive(Clone, Copy, Debug)]
pub struct Quote {
    /// 回复引用行，如「在 {date}，{sender} 写道：」。不含前后换行。
    pub reply_attribution: fn(date: &str, sender: &str) -> String,
    /// 转发头：分隔线与发件人/日期/主题/收件人四行，以 `\n` 连接，不含前后换行。
    pub forward_header: fn(sender: &str, date: &str, subject: &str, recipient: &str) -> String,
    /// 引用行里给人读的本地日期时间。
    pub attribution_date: fn(date: &AttributionDate) -> String,
}

/// [`Quote::attribution_date`] 的输入：已按本地时区拆好的日历时间。
///
/// 字段与 `maild::localtime::LocalDateTime` 对应，本 crate 不依赖 `maild`，由调用方逐字段搬运。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttributionDate {
    pub year: i32,
    /// `1..=12`
    pub month: u8,
    /// `1..=31`
    pub day: u8,
    /// `0..=6`（`0` = 周日 .. `6` = 周六）；越界按周六处理，与今天的行为一致。
    pub weekday: u8,
    /// `0..=23`
    pub hour: u8,
    /// `0..=59`
    pub minute: u8,
    /// 相对 UTC 的分钟偏移（东八区为 `480`）。
    pub utc_offset_minutes: i32,
}

impl AttributionDate {
    /// `(符号, 小时, 分钟)`，供各语言格式化 `UTC±HH:MM`。
    fn utc_offset(&self) -> (char, u32, u32) {
        let sign = if self.utc_offset_minutes >= 0 {
            '+'
        } else {
            '-'
        };
        let abs = self.utc_offset_minutes.unsigned_abs();
        (sign, abs / 60, abs % 60)
    }

    fn weekday_index(&self) -> usize {
        usize::from(self.weekday).min(6)
    }
}

/// 菜单栏 companion（`maild-menu`）。状态栏按钮的提示与菜单标题就是产品名，不在此列。
#[derive(Clone, Copy, Debug)]
pub struct Menu {
    pub open_inbox: &'static str,
    pub sync_now: &'static str,
    pub compose: &'static str,
    pub settings: &'static str,
    pub background_and_notifications: &'static str,
    pub repair_and_background: &'static str,
    /// 「退出 {product}」：停掉整个受管 Agent。
    pub quit: fn(product: &str) -> String,
}

/// 原生启动器与修复对话框（`maild-launcher`）。
///
/// 结果与报错文案会原样显示在对话框里，所以也在这里；只进 `eprintln!` 的不在此列。
#[derive(Clone, Copy, Debug)]
pub struct Launcher {
    // —— 窗口与状态 ——
    /// 对话框标题。
    pub window_title: fn(product: &str) -> String,
    /// 状态摘要：后台、邮件前端、开机启动三行，空一行后接一句提示。
    pub status_summary: fn(background: &str, mailbox: &str, login: &str, note: &str) -> String,
    pub status_online: &'static str,
    pub status_offline: &'static str,
    pub mailbox_ready: &'static str,
    pub mailbox_needs_check: &'static str,
    pub note_closing_keeps_receiving: &'static str,
    /// 不在完整 app bundle 内运行时的提示；参数是 app bundle 名。
    pub note_open_from_full_app: fn(app_bundle: &str) -> String,

    // —— 按钮 ——
    pub button_open_mailbox: &'static str,
    pub button_repair_options: &'static str,
    pub button_close: &'static str,
    pub button_restore_builtin: &'static str,
    pub button_background: &'static str,
    pub button_back: &'static str,
    pub button_stop_background: &'static str,
    pub button_start_once: &'static str,
    pub button_login_item: &'static str,
    pub button_launch_at_login: &'static str,
    pub button_system_login_items: &'static str,
    pub button_enable: &'static str,
    pub button_disable: &'static str,
    pub button_cancel: &'static str,
    pub button_continue: &'static str,
    pub button_ok: &'static str,

    // —— 说明与确认 ——
    pub repair_options_prompt: &'static str,
    pub confirm_restore_title: &'static str,
    pub confirm_restore_body: &'static str,
    pub confirm_stop_title: &'static str,
    pub confirm_stop_body: &'static str,
    pub login_prompt: &'static str,
    pub login_choice_prompt: &'static str,

    // —— 开机启动（登录项）状态 ——
    pub login_isolated: &'static str,
    pub login_registered: &'static str,
    pub login_requires_approval: &'static str,
    pub login_not_registered: &'static str,
    pub login_unavailable: &'static str,
    /// 非 macOS：状态与注册/撤销的报错共用。
    pub login_unsupported_platform: &'static str,
    /// 不在完整 app bundle 内时的登录项状态；参数是 app bundle 名。
    pub login_manage_from_app: fn(app_bundle: &str) -> String,

    // —— 操作结果 ——
    pub result_mailbox_requested: &'static str,
    pub result_mailbox_queued: &'static str,
    pub result_repaired_online: &'static str,
    pub result_repaired_offline: &'static str,
    pub result_background_connected: &'static str,
    pub result_start_requested: &'static str,
    pub result_stop_accepted: &'static str,
    pub result_login_register_requested: &'static str,
    pub result_login_unregistered: &'static str,
    pub result_login_settings_requested: &'static str,

    // —— 操作失败 ——
    pub error_unsupported_by_agent: &'static str,
    pub error_connection_setup: &'static str,
    pub error_app_incomplete: &'static str,
    pub error_local_connection_unavailable: &'static str,
    pub error_operation_unconfirmed: &'static str,
    pub error_repair_incomplete: &'static str,
    pub error_agent_unconfirmed: &'static str,
    pub error_custom_connection: &'static str,
    pub error_open_from_full_app: &'static str,
    pub error_mailbox_unavailable: &'static str,
    pub error_mailbox_cannot_open: &'static str,
    pub error_repair_unconfirmed: &'static str,
    pub error_repair_offline_failed: &'static str,
    pub error_platform_dirs_unavailable: &'static str,
    pub error_background_busy: &'static str,
    pub error_background_config_unavailable: &'static str,
    pub error_start_failed: &'static str,
    pub error_stop_unconfirmed: &'static str,
    pub error_login_isolated_register: &'static str,
    pub error_login_register_failed: &'static str,
    pub error_login_isolated_unregister: &'static str,
    pub error_login_unregister_failed: &'static str,
    pub error_agent_path_invalid: &'static str,
    pub error_agent_launch_failed: &'static str,
    pub error_agent_launch_unsupported: &'static str,
    pub error_open_mail_window_failed: &'static str,
    pub error_in_place_unsupported: &'static str,
}

#[cfg(test)]
mod tests;
