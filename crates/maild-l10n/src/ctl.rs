//! 命令行（`maildctl` / `maild` 维护子命令）给人读的结果、确认、警告与输入提示（ADR-0048 L5）。
//!
//! 两种语言都写在本文件，避免与其他分组的并行改动冲突；`zh_hans.rs` / `en.rs` 各只引用一行。
//!
//! 不在此列（见 crate 文档的范围）：`--json` 输出、`key: value` 形式的字段名与取值、
//! `--help` 文案，以及进入 `AppError` 的解析/校验/IO 报错——后者也是 `--json` 错误行的
//! `message`，面向开发者，与协议 `error.message` 同属非目标。

/// 命令行文案。字段名即消息；带参数的是 `fn` 字段，调用写作 `(m.ctl.plugin_removed)(id)`。
#[derive(Clone, Copy, Debug)]
pub struct Ctl {
    // —— 插件事务（`maildctl plugin …`） ——
    /// 首次安装成功：「已安装 {id} {version}」。
    pub plugin_installed: fn(id: &str, version: &str) -> String,
    /// 升级成功，旧版进入回滚位。
    pub plugin_upgraded: fn(id: &str, from_version: &str, version: &str) -> String,
    /// 移除成功。
    pub plugin_removed: fn(id: &str) -> String,
    /// 回滚成功：当前版本与回滚位互换。
    pub plugin_rolled_back: fn(id: &str, version: &str) -> String,
    /// `plugin enable` 之后接在 id 后面的状态词。
    pub plugin_enabled: &'static str,
    /// `plugin disable` 之后接在 id 后面的状态词。
    pub plugin_disabled: &'static str,

    // —— 凭证与协议代 ——
    /// `plugin-token` 轮换后、打印明文之前写到 stderr 的提醒。
    pub token_rotated_notice: &'static str,
    /// 后台内核协议代比本 CLI 旧；参数是 CLI 期望的协议代。
    pub kernel_outdated_warning: fn(expected: u64) -> String,
    /// 本 CLI 比后台内核旧；参数是 CLI 名与产品名。
    pub app_outdated_warning: fn(cli: &str, product: &str) -> String,

    // —— 草稿、阅读策略、语言偏好 ——
    /// `draft list` 中没有主题时的占位。
    pub draft_no_subject: &'static str,
    /// 草稿附件引用的源 part 已不可读时的行尾标记（调用方在前面加一个空格）。
    pub draft_attachment_unreadable: &'static str,
    /// `reading-policy` 读到停摆的策略文件时的整行提示。
    pub reading_policy_not_persisted: &'static str,
    /// `locale` 读到停摆的偏好文件时的整行提示。
    pub locale_not_persisted: &'static str,

    // —— 账户 ——
    /// `account test` / `provider-test` 探测未通过时的结论（退出码 4）。
    pub probe_failed: &'static str,
    /// 秘密输入提示（TTY 上关闭回显时显示）；调用方在后面补一个空格。
    pub prompt_imap_password: &'static str,
    pub prompt_smtp_password: &'static str,
    pub prompt_new_imap_password: &'static str,
    pub prompt_new_smtp_password: &'static str,

    // —— `maild backup` / `restore` / `paths` ——
    /// 备份完成；参数是调用方按 `{:?}` 格式化好的输出路径。
    pub backup_done: fn(output: &str) -> String,
    /// 备份失败；参数是已净化的报错详情（详情本身不翻译）。
    pub backup_failed: fn(detail: &str) -> String,
    /// 恢复完成；参数是调用方按 `{:?}` 格式化好的数据库与回滚快照路径。
    pub restore_done: fn(database: &str, rollback: &str) -> String,
    /// 恢复失败；参数是已净化的报错详情。
    pub restore_failed: fn(detail: &str) -> String,
    /// `paths` 子命令找不到平台目录；参数是已净化的报错详情。
    pub paths_discovery_failed: fn(detail: &str) -> String,

    // —— 账户状态原因（ADR-0048 L6） ——
    /// `accounts` / `account list` 的离线原因按 daemon 给的 `detail_code` 成文。
    pub account_detail: AccountDetailTexts,
}

/// 账户状态原因的文案，字段与 daemon 的 `AccountDetailCode` 一一对应（`maild` 的测试核对没有遗漏）。
/// 服务器原文由调用方接在后面，不翻译。
#[derive(Clone, Copy, Debug)]
pub struct AccountDetailTexts {
    pub credential_unavailable: &'static str,
    pub credential_invalid: &'static str,
    pub connect_failed: &'static str,
    pub connect_timeout: &'static str,
    pub tls_failed: &'static str,
    pub tls_timeout: &'static str,
    pub starttls_unsupported: &'static str,
    pub greeting_invalid: &'static str,
    pub login_failed: &'static str,
    pub login_timeout: &'static str,
    pub idle_failed: &'static str,
    pub idle_timeout: &'static str,
    pub sync_failed: &'static str,
    pub poll_failed: &'static str,
    pub provider_unavailable: &'static str,
    pub provider_timeout: &'static str,
    pub provider_failed: &'static str,
    pub provider_response_invalid: &'static str,
    pub internal: &'static str,
    /// 新 daemon 给了本 CLI 认不出的 code。
    pub unknown: &'static str,
}

impl AccountDetailTexts {
    /// 认得的 code 的文案；认不出时为 `None`。
    pub fn known(&self, code: &str) -> Option<&'static str> {
        Some(match code {
            "credential_unavailable" => self.credential_unavailable,
            "credential_invalid" => self.credential_invalid,
            "connect_failed" => self.connect_failed,
            "connect_timeout" => self.connect_timeout,
            "tls_failed" => self.tls_failed,
            "tls_timeout" => self.tls_timeout,
            "starttls_unsupported" => self.starttls_unsupported,
            "greeting_invalid" => self.greeting_invalid,
            "login_failed" => self.login_failed,
            "login_timeout" => self.login_timeout,
            "idle_failed" => self.idle_failed,
            "idle_timeout" => self.idle_timeout,
            "sync_failed" => self.sync_failed,
            "poll_failed" => self.poll_failed,
            "provider_unavailable" => self.provider_unavailable,
            "provider_timeout" => self.provider_timeout,
            "provider_failed" => self.provider_failed,
            "provider_response_invalid" => self.provider_response_invalid,
            "internal" => self.internal,
            _ => return None,
        })
    }

    /// 按 code 成文，认不出的 code 用通用说明。
    pub fn text(&self, code: &str) -> &'static str {
        self.known(code).unwrap_or(self.unknown)
    }
}

const ZH_ACCOUNT_DETAIL: AccountDetailTexts = AccountDetailTexts {
    credential_unavailable: "无法读取已保存的密码",
    credential_invalid: "已保存的密码无法使用",
    connect_failed: "无法连接服务器",
    connect_timeout: "连接服务器超时",
    tls_failed: "无法建立安全连接（TLS）",
    tls_timeout: "建立安全连接超时",
    starttls_unsupported: "服务器不支持 STARTTLS",
    greeting_invalid: "服务器没有给出有效的 IMAP 问候",
    login_failed: "服务器拒绝了登录",
    login_timeout: "登录超时",
    idle_failed: "实时推送（IDLE）中断",
    idle_timeout: "实时推送（IDLE）无响应",
    sync_failed: "同步邮件失败",
    poll_failed: "定时收信失败",
    provider_unavailable: "收信插件不可用",
    provider_timeout: "收信插件响应超时",
    provider_failed: "收信插件报告了错误",
    provider_response_invalid: "收信插件返回了无效数据",
    internal: "内部错误",
    unknown: "连接出现问题",
};

const EN_ACCOUNT_DETAIL: AccountDetailTexts = AccountDetailTexts {
    credential_unavailable: "Couldn't read the saved password",
    credential_invalid: "The saved password can't be used",
    connect_failed: "Couldn't connect to the server",
    connect_timeout: "Connecting to the server timed out",
    tls_failed: "Couldn't set up a secure (TLS) connection",
    tls_timeout: "Setting up a secure connection timed out",
    starttls_unsupported: "The server doesn't support STARTTLS",
    greeting_invalid: "The server didn't send a valid IMAP greeting",
    login_failed: "The server rejected the sign-in",
    login_timeout: "Signing in timed out",
    idle_failed: "Live updates (IDLE) stopped",
    idle_timeout: "Live updates (IDLE) stopped responding",
    sync_failed: "Syncing mail failed",
    poll_failed: "Scheduled mail check failed",
    provider_unavailable: "The mail plugin isn't available",
    provider_timeout: "The mail plugin didn't respond in time",
    provider_failed: "The mail plugin reported an error",
    provider_response_invalid: "The mail plugin returned invalid data",
    internal: "Internal error",
    unknown: "Connection problem",
};

/// 简体中文。与迁移前 `maildctl.rs` / `maild.rs` 中的字面量逐字节一致。
pub(crate) const ZH_HANS: Ctl = Ctl {
    plugin_installed: zh::plugin_installed,
    plugin_upgraded: zh::plugin_upgraded,
    plugin_removed: zh::plugin_removed,
    plugin_rolled_back: zh::plugin_rolled_back,
    plugin_enabled: "已启用",
    plugin_disabled: "已停用",

    token_rotated_notice:
        "凭证已轮换；旧凭证对新连接立即失效。请立即写入插件自己的配置，勿入 shell 历史：",
    kernel_outdated_warning: zh::kernel_outdated_warning,
    app_outdated_warning: zh::app_outdated_warning,

    draft_no_subject: "（无主题）",
    draft_attachment_unreadable: "[源 part 已不可读]",
    reading_policy_not_persisted: "persisted: false（策略文件停摆：读到的是缺省值，写会被拒绝）",
    locale_not_persisted: "persisted: false（偏好文件停摆：读到的是缺省值，写会被拒绝）",

    probe_failed: "探测未通过",
    prompt_imap_password: "IMAP 密码",
    prompt_smtp_password: "SMTP 密码",
    prompt_new_imap_password: "新的 IMAP 密码",
    prompt_new_smtp_password: "新的 SMTP 密码",

    backup_done: zh::backup_done,
    backup_failed: zh::backup_failed,
    restore_done: zh::restore_done,
    restore_failed: zh::restore_failed,
    paths_discovery_failed: zh::paths_discovery_failed,

    account_detail: ZH_ACCOUNT_DETAIL,
};

/// 英文。
pub(crate) const EN: Ctl = Ctl {
    plugin_installed: en::plugin_installed,
    plugin_upgraded: en::plugin_upgraded,
    plugin_removed: en::plugin_removed,
    plugin_rolled_back: en::plugin_rolled_back,
    plugin_enabled: "enabled",
    plugin_disabled: "disabled",

    token_rotated_notice: "Token rotated; the old token stops working for new connections \
         immediately. Save it in the plugin's own configuration now and keep it out of your \
         shell history:",
    kernel_outdated_warning: en::kernel_outdated_warning,
    app_outdated_warning: en::app_outdated_warning,

    draft_no_subject: "(No Subject)",
    draft_attachment_unreadable: "[source part no longer readable]",
    reading_policy_not_persisted:
        "persisted: false (policy file unusable: showing defaults; changes will be refused)",
    locale_not_persisted:
        "persisted: false (preference file unusable: showing defaults; changes will be refused)",

    probe_failed: "probe did not pass",
    prompt_imap_password: "IMAP password:",
    prompt_smtp_password: "SMTP password:",
    prompt_new_imap_password: "New IMAP password:",
    prompt_new_smtp_password: "New SMTP password:",

    backup_done: en::backup_done,
    backup_failed: en::backup_failed,
    restore_done: en::restore_done,
    restore_failed: en::restore_failed,
    paths_discovery_failed: en::paths_discovery_failed,

    account_detail: EN_ACCOUNT_DETAIL,
};

mod zh {
    pub(super) fn plugin_installed(id: &str, version: &str) -> String {
        format!("已安装 {id} {version}")
    }

    pub(super) fn plugin_upgraded(id: &str, from_version: &str, version: &str) -> String {
        format!("已升级 {id}: {from_version} -> {version}（旧版在回滚位，可用 rollback 切回）")
    }

    pub(super) fn plugin_removed(id: &str) -> String {
        format!("已移除 {id}")
    }

    pub(super) fn plugin_rolled_back(id: &str, version: &str) -> String {
        format!("{id} 已切换到 {version}（再执行一次切回另一版）")
    }

    pub(super) fn kernel_outdated_warning(expected: u64) -> String {
        format!("警告: 后台内核协议代较旧（期望 {expected}），结果可能不可靠；请重启后台内核")
    }

    pub(super) fn app_outdated_warning(cli: &str, product: &str) -> String {
        format!("警告: {cli} 较旧（内核协议代更新），建议更新 {product}")
    }

    pub(super) fn backup_done(output: &str) -> String {
        format!("备份完成: {output}")
    }

    pub(super) fn backup_failed(detail: &str) -> String {
        format!("备份失败: {detail}")
    }

    pub(super) fn restore_done(database: &str, rollback: &str) -> String {
        format!("恢复完成: {database}；回滚快照: {rollback}")
    }

    pub(super) fn restore_failed(detail: &str) -> String {
        format!("恢复失败: {detail}")
    }

    pub(super) fn paths_discovery_failed(detail: &str) -> String {
        format!("路径发现失败: {detail}")
    }
}

mod en {
    pub(super) fn plugin_installed(id: &str, version: &str) -> String {
        format!("Installed {id} {version}")
    }

    pub(super) fn plugin_upgraded(id: &str, from_version: &str, version: &str) -> String {
        format!(
            "Upgraded {id}: {from_version} -> {version} (the previous version is in the \
             rollback slot; use rollback to switch back)"
        )
    }

    pub(super) fn plugin_removed(id: &str) -> String {
        format!("Removed {id}")
    }

    pub(super) fn plugin_rolled_back(id: &str, version: &str) -> String {
        format!("{id} switched to {version} (run rollback again to switch back)")
    }

    pub(super) fn kernel_outdated_warning(expected: u64) -> String {
        format!(
            "Warning: the background service uses an older protocol version (expected \
             {expected}), so results may be unreliable; restart the background service"
        )
    }

    pub(super) fn app_outdated_warning(cli: &str, product: &str) -> String {
        format!(
            "Warning: {cli} is older than the background service's protocol; consider \
             updating {product}"
        )
    }

    pub(super) fn backup_done(output: &str) -> String {
        format!("Backup complete: {output}")
    }

    pub(super) fn backup_failed(detail: &str) -> String {
        format!("Backup failed: {detail}")
    }

    pub(super) fn restore_done(database: &str, rollback: &str) -> String {
        format!("Restore complete: {database}; rollback snapshot: {rollback}")
    }

    pub(super) fn restore_failed(detail: &str) -> String {
        format!("Restore failed: {detail}")
    }

    pub(super) fn paths_discovery_failed(detail: &str) -> String {
        format!("Could not determine platform directories: {detail}")
    }
}

#[cfg(test)]
mod tests {
    use crate::{EN, ZH_HANS};

    /// 中文与迁移前的 `format!` 模板逐字节一致；英文给一份快照。
    #[test]
    fn parameterized_snapshot() {
        let zh = ZH_HANS.ctl;
        assert_eq!((zh.plugin_installed)("a.b", "1.0.0"), "已安装 a.b 1.0.0");
        assert_eq!(
            (zh.plugin_upgraded)("a.b", "1.0.0", "1.1.0"),
            format!(
                "已升级 {}: {} -> {}（旧版在回滚位，可用 rollback 切回）",
                "a.b", "1.0.0", "1.1.0"
            )
        );
        assert_eq!((zh.plugin_removed)("a.b"), "已移除 a.b");
        assert_eq!(
            (zh.plugin_rolled_back)("a.b", "1.0.0"),
            "a.b 已切换到 1.0.0（再执行一次切回另一版）"
        );
        assert_eq!(
            (zh.kernel_outdated_warning)(1),
            "警告: 后台内核协议代较旧（期望 1），结果可能不可靠；请重启后台内核"
        );
        assert_eq!(
            (zh.app_outdated_warning)("featherpostctl", "Featherpost"),
            "警告: featherpostctl 较旧（内核协议代更新），建议更新 Featherpost"
        );
        assert_eq!(
            (zh.backup_done)("\"/b.sqlite3\""),
            "备份完成: \"/b.sqlite3\""
        );
        assert_eq!((zh.backup_failed)("x"), "备份失败: x");
        assert_eq!(
            (zh.restore_done)("\"/a\"", "\"/r\""),
            "恢复完成: \"/a\"；回滚快照: \"/r\""
        );
        assert_eq!((zh.restore_failed)("x"), "恢复失败: x");
        assert_eq!((zh.paths_discovery_failed)("x"), "路径发现失败: x");

        let en = EN.ctl;
        assert_eq!((en.plugin_installed)("a.b", "1.0.0"), "Installed a.b 1.0.0");
        assert_eq!(
            (en.plugin_upgraded)("a.b", "1.0.0", "1.1.0"),
            "Upgraded a.b: 1.0.0 -> 1.1.0 (the previous version is in the rollback slot; \
             use rollback to switch back)"
        );
        assert_eq!((en.plugin_removed)("a.b"), "Removed a.b");
        assert_eq!(
            (en.plugin_rolled_back)("a.b", "1.0.0"),
            "a.b switched to 1.0.0 (run rollback again to switch back)"
        );
        assert_eq!(
            (en.kernel_outdated_warning)(1),
            "Warning: the background service uses an older protocol version (expected 1), \
             so results may be unreliable; restart the background service"
        );
        assert_eq!(
            (en.app_outdated_warning)("featherpostctl", "Featherpost"),
            "Warning: featherpostctl is older than the background service's protocol; \
             consider updating Featherpost"
        );
        assert_eq!(
            (en.restore_done)("\"/a\"", "\"/r\""),
            "Restore complete: \"/a\"; rollback snapshot: \"/r\""
        );
    }

    /// 英文目录里不该漏进中文（缺字段已由编译保证，这里防的是复制粘贴漏翻）。
    #[test]
    fn english_static_strings_are_ascii() {
        let en = EN.ctl;
        for text in [
            en.plugin_enabled,
            en.plugin_disabled,
            en.token_rotated_notice,
            en.draft_no_subject,
            en.draft_attachment_unreadable,
            en.reading_policy_not_persisted,
            en.locale_not_persisted,
            en.probe_failed,
            en.prompt_imap_password,
            en.prompt_smtp_password,
            en.prompt_new_imap_password,
            en.prompt_new_smtp_password,
        ] {
            assert!(text.is_ascii(), "{text}");
        }
        assert_eq!(ZH_HANS.ctl.plugin_enabled, "已启用");
        assert_eq!(ZH_HANS.ctl.draft_no_subject, "（无主题）");
    }

    /// 账户状态原因：认得的 code 各有文案，认不出的回落通用说明（全量核对在 `maild` 的测试里，
    /// 那里才有 daemon 的 code 清单）。
    #[test]
    fn account_detail_falls_back_for_unknown_codes() {
        for ctl in [ZH_HANS.ctl, EN.ctl] {
            let texts = ctl.account_detail;
            assert_eq!(texts.known("future_code"), None);
            assert_eq!(texts.text("future_code"), texts.unknown);
            assert_eq!(texts.text("login_failed"), texts.login_failed);
        }
        assert_eq!(EN.ctl.account_detail.login_failed, "The server rejected the sign-in");
        assert!(EN.ctl.account_detail.unknown.is_ascii());
    }
}
