use crate::{Controller, Operation, Status};
use rfd::MessageLevel;
#[cfg(not(target_os = "macos"))]
use rfd::{MessageButtons, MessageDialog, MessageDialogResult};

fn title() -> String {
    format!("{} 修复与后台管理", maild_brand::PRODUCT_NAME)
}

pub fn show_recovery(controller: &mut dyn Controller) {
    super::platform::prepare_ui();
    loop {
        let status = controller.status();
        match choose(&status_text(&status), ["打开邮箱", "修复选项…", "关闭"]) {
            Some(0) => result(controller.perform(Operation::OpenMailbox)),
            Some(1) => repair_options(controller),
            _ => break,
        }
    }
}

fn status_text(status: &Status) -> String {
    format!(
        "后台：{}\n邮件前端：{}\n开机启动：{}\n\n{}",
        if status.online {
            "已连接"
        } else {
            "未连接"
        },
        if status.mailbox_available {
            "已配置阅读入口"
        } else {
            "需要检查"
        },
        status.login,
        if status.can_manage {
            "关闭此窗口不会停止收信。".to_owned()
        } else {
            format!(
                "请从完整 {} 打开，以管理后台和恢复邮件前端。",
                maild_brand::APP_BUNDLE_NAME
            )
        }
    )
}

fn repair_options(controller: &mut dyn Controller) {
    loop {
        match choose("邮件窗口打不开时，可从安装包恢复内置前端。\n\n邮件、账户与凭据保持不变；现有前端包保留备份。", ["恢复内置前端", "后台管理…", "返回"]) {
            Some(0) => {
                if confirm("恢复内置前端？", "将恢复安装包自带的邮件前端，并设为默认。现有前端包会保留备份。\n\n继续前请关闭邮件阅读窗口。") {
                    result(controller.perform(Operation::Repair));
                }
            }
            Some(1) => background(controller),
            _ => break,
        }
    }
}

fn background(controller: &mut dyn Controller) {
    loop {
        let status = controller.status();
        let action = if status.online {
            "停止后台"
        } else {
            "启动一次"
        };
        match choose(&status_text(&status), [action, "登录项…", "返回"]) {
            Some(0) if status.online => {
                if confirm(
                    "停止收信后台？",
                    "收信、规则和系统通知将暂停。可以从此窗口再次启动后台。",
                ) {
                    result(controller.perform(Operation::Stop));
                }
            }
            Some(0) => result(controller.perform(Operation::StartOnce)),
            Some(1) => login(controller),
            _ => break,
        }
    }
}

fn login(controller: &mut dyn Controller) {
    match choose(
        "开机启动由系统管理。注册或撤销后，可在系统登录项设置中检查。",
        ["开机启动…", "系统登录项…", "返回"],
    ) {
        Some(0) => match choose(
            "选择开机启动行为。此处不会改变邮件或账户。",
            ["启用", "停用", "取消"],
        ) {
            Some(0) => result(controller.perform(Operation::RegisterLogin)),
            Some(1) => result(controller.perform(Operation::UnregisterLogin)),
            _ => {}
        },
        Some(1) => result(controller.perform(Operation::SystemSettings)),
        _ => {}
    }
}

fn choose(description: &str, labels: [&str; 3]) -> Option<usize> {
    dialog(&title(), description, MessageLevel::Info, &labels)
}

fn dialog(title: &str, description: &str, level: MessageLevel, labels: &[&str]) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        super::platform::dialog(title, description, level, labels)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let buttons = match labels {
            [one] => MessageButtons::OkCustom((*one).into()),
            [one, two] => MessageButtons::OkCancelCustom((*one).into(), (*two).into()),
            [one, two, three] => {
                MessageButtons::YesNoCancelCustom((*one).into(), (*two).into(), (*three).into())
            }
            _ => return None,
        };
        let response = MessageDialog::new()
            .set_title(title)
            .set_description(description)
            .set_level(level)
            .set_buttons(buttons)
            .show();
        match response {
            MessageDialogResult::Custom(text) => labels.iter().position(|label| *label == text),
            _ => None,
        }
    }
}

fn confirm(title: &str, description: &str) -> bool {
    dialog(title, description, MessageLevel::Warning, &["继续", "取消"]) == Some(0)
}

fn result(outcome: Result<&str, &str>) {
    let (text, level) = match outcome {
        Ok(text) => (text, MessageLevel::Info),
        Err(text) => (text, MessageLevel::Error),
    };
    dialog(&title(), text, level, &["好"]);
}

pub(crate) fn show_error(message: &str) {
    super::platform::prepare_ui();
    result(Err(message));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disconnected_state_never_claims_mailbox_or_background_is_running() {
        let text = status_text(&Status {
            online: false,
            mailbox_available: false,
            can_manage: false,
            login: "未注册".into(),
        });
        assert!(
            text.contains("未连接") && text.contains("需要检查") && text.contains(&format!("完整 {}", maild_brand::APP_BUNDLE_NAME))
        );
        assert!(!text.contains("关闭此窗口不会停止收信"));
    }
}
