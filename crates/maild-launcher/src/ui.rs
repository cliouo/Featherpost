use crate::l10n::text;
use crate::{Controller, Operation, Status};
use maild_l10n::Launcher;
use rfd::MessageLevel;
#[cfg(not(target_os = "macos"))]
use rfd::{MessageButtons, MessageDialog, MessageDialogResult};

fn title() -> String {
    (text().window_title)(maild_brand::PRODUCT_NAME)
}

pub fn show_recovery(controller: &mut dyn Controller) -> Option<std::path::PathBuf> {
    super::platform::prepare_ui();
    loop {
        // `status()` also refreshes the language from the daemon, so read the strings after it.
        let status = controller.status();
        let t = text();
        match choose(
            &status_text(t, &status),
            [t.button_open_mailbox, t.button_repair_options, t.button_close],
        ) {
            Some(0) => match controller.mailbox_host() {
                Ok(Some(exe)) => return Some(exe),
                Ok(None) => result(controller.perform(Operation::OpenMailbox)),
                Err(message) => result(Err(message)),
            },
            Some(1) => repair_options(controller),
            _ => break,
        }
    }
    None
}

fn status_text(t: &Launcher, status: &Status) -> String {
    let note = if status.can_manage {
        t.note_closing_keeps_receiving.to_owned()
    } else {
        (t.note_open_from_full_app)(maild_brand::APP_BUNDLE_NAME)
    };
    (t.status_summary)(
        if status.online {
            t.status_online
        } else {
            t.status_offline
        },
        if status.mailbox_available {
            t.mailbox_ready
        } else {
            t.mailbox_needs_check
        },
        &status.login,
        &note,
    )
}

fn repair_options(controller: &mut dyn Controller) {
    loop {
        let t = text();
        match choose(
            t.repair_options_prompt,
            [t.button_restore_builtin, t.button_background, t.button_back],
        ) {
            Some(0) => {
                if confirm(t.confirm_restore_title, t.confirm_restore_body) {
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
        let t = text();
        let action = if status.online {
            t.button_stop_background
        } else {
            t.button_start_once
        };
        match choose(
            &status_text(t, &status),
            [action, t.button_login_item, t.button_back],
        ) {
            Some(0) if status.online => {
                if confirm(t.confirm_stop_title, t.confirm_stop_body) {
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
    let t = text();
    match choose(
        t.login_prompt,
        [
            t.button_launch_at_login,
            t.button_system_login_items,
            t.button_back,
        ],
    ) {
        Some(0) => match choose(
            t.login_choice_prompt,
            [t.button_enable, t.button_disable, t.button_cancel],
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
    let t = text();
    dialog(
        title,
        description,
        MessageLevel::Warning,
        &[t.button_continue, t.button_cancel],
    ) == Some(0)
}

fn result(outcome: Result<&str, &str>) {
    let (text, level) = match outcome {
        Ok(text) => (text, MessageLevel::Info),
        Err(text) => (text, MessageLevel::Error),
    };
    dialog(&title(), text, level, &[super::l10n::text().button_ok]);
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
        let status = Status {
            online: false,
            mailbox_available: false,
            can_manage: false,
            login: "未注册".into(),
        };
        let text = status_text(&maild_l10n::ZH_HANS.launcher, &status);
        assert!(
            text.contains("未连接") && text.contains("需要检查") && text.contains(&format!("完整 {}", maild_brand::APP_BUNDLE_NAME))
        );
        assert!(!text.contains("关闭此窗口不会停止收信"));

        let en = &maild_l10n::EN.launcher;
        let text = status_text(en, &status);
        assert!(text.contains(en.status_offline) && text.contains(en.mailbox_needs_check));
        assert!(text.contains(maild_brand::APP_BUNDLE_NAME));
        assert!(!text.contains(en.note_closing_keeps_receiving));
        assert!(!text.contains("未连接"), "{text}");
    }

    #[test]
    fn manageable_online_state_uses_the_reassuring_note_in_both_languages() {
        let status = Status {
            online: true,
            mailbox_available: true,
            can_manage: true,
            login: "已注册".into(),
        };
        assert_eq!(
            status_text(&maild_l10n::ZH_HANS.launcher, &status),
            "后台：已连接\n邮件前端：已配置阅读入口\n开机启动：已注册\n\n关闭此窗口不会停止收信。"
        );
        let en = &maild_l10n::EN.launcher;
        let text = status_text(en, &status);
        assert!(text.contains(en.status_online) && text.contains(en.mailbox_ready));
        assert!(text.ends_with(en.note_closing_keeps_receiving));
    }
}
