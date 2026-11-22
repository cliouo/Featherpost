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
}

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
        assert_eq!((zh.backup_done)("\"/b.sqlite3\""), "备份完成: \"/b.sqlite3\"");
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
}
