//! 英文。菜单与按钮按 macOS 习惯用标题式大小写。

use crate::{AttributionDate, Launcher, Menu, Messages, Notifications, Quote};

pub const EN: Messages = Messages {
    notifications: Notifications {
        otp_title: "Verification Code",
        otp_body,
        new_mail_title: "New Mail",
        new_mail_burst_body,
        new_mail_masked_body,
        no_subject: "(No Subject)",
    },
    quote: Quote {
        reply_attribution,
        forward_header,
        attribution_date,
    },
    menu: Menu {
        open_inbox: "Open Inbox",
        sync_now: "Sync Mail Now",
        compose: "New Message…",
        settings: "Settings…",
        background_and_notifications: "Background & Notifications…",
        repair_and_background: "Repair & Background…",
        quit,
    },
    launcher: Launcher {
        window_title,
        status_summary,
        status_online: "Connected",
        status_offline: "Not connected",
        mailbox_ready: "Ready to open",
        mailbox_needs_check: "Needs attention",
        note_closing_keeps_receiving: "Closing this window does not stop receiving mail.",
        note_open_from_full_app,

        button_open_mailbox: "Open Mailbox",
        button_repair_options: "Repair Options…",
        button_close: "Close",
        button_restore_builtin: "Restore Built-in Frontend",
        button_background: "Background…",
        button_back: "Back",
        button_stop_background: "Stop Background",
        button_start_once: "Start Once",
        button_login_item: "Login Item…",
        button_launch_at_login: "Launch at Login…",
        button_system_login_items: "System Login Items…",
        button_enable: "Enable",
        button_disable: "Disable",
        button_cancel: "Cancel",
        button_continue: "Continue",
        button_ok: "OK",

        repair_options_prompt: "If the mail window won't open, you can restore the built-in frontend from the installer.\n\nYour mail, accounts, and credentials stay unchanged; the current frontend package is kept as a backup.",
        confirm_restore_title: "Restore the built-in frontend?",
        confirm_restore_body: "The mail frontend that ships with the installer will be restored and set as the default. The current frontend package will be kept as a backup.\n\nClose any mail reading windows before you continue.",
        confirm_stop_title: "Stop the mail background?",
        confirm_stop_body: "Receiving mail, rules, and system notifications will pause. You can start the background again from this window.",
        login_prompt: "Launch at login is managed by the system. After registering or unregistering, you can check it in the system's Login Items settings.",
        login_choice_prompt: "Choose what happens at login. This does not change your mail or accounts.",

        login_isolated: "Isolated directory: system login items are left unchanged",
        login_registered: "Registered",
        login_requires_approval: "Waiting for system approval",
        login_not_registered: "Not registered",
        login_unavailable: "Unavailable",
        login_unsupported_platform: "Login item management is not available on this platform yet",
        login_manage_from_app,

        result_mailbox_requested: "Asked to open your mailbox. Once the mail window is open, you can close the repair window.",
        result_mailbox_queued: "The open request is queued. Switch back to the mail window that is already running.",
        result_repaired_online: "The built-in frontend has been restored and set as the default. The previous package was kept as a backup.",
        result_repaired_offline: "The built-in frontend has been restored and set as the default. Start the background to open your mailbox.",
        result_background_connected: "Background connected",
        result_start_requested: "Asked to start once. Go back to the main window to refresh the connection status.",
        result_stop_accepted: "The background accepted the stop request and is saving its state before exiting.",
        result_login_register_requested: "Launch at login was requested. If the system asks for approval, open the system's Login Items settings.",
        result_login_unregistered: "Launch at login was unregistered. Go back to check the background status.",
        result_login_settings_requested: "Asked to open the system's Login Items settings.",

        error_unsupported_by_agent: "The background doesn't support this action. Quit the old background and use the complete, updated app.",
        error_connection_setup: "Couldn't set up the local background connection.",
        error_app_incomplete: "The app is incomplete, so the background can't be managed safely.",
        error_local_connection_unavailable: "The local connection is unavailable. Open this from the complete app bundle.",
        error_operation_unconfirmed: "The action isn't confirmed yet. Refresh the status; don't run the repair again.",
        error_repair_incomplete: "The repair wasn't fully confirmed. Check the installer, files in use, and free disk space; once disk space is available, restart the background and check again. Don't retry automatically.",
        error_agent_unconfirmed: "The background didn't confirm the action. Refresh the status and check the version; a repair or stop request may still be running, so don't retry automatically.",
        error_custom_connection: "This entry uses a custom connection and can't manage the platform background.",
        error_open_from_full_app: "Open Repair & Background from the complete app bundle.",
        error_mailbox_unavailable: "The mail frontend isn't available yet. Restore the built-in frontend or start the background first.",
        error_mailbox_cannot_open: "The current frontend can't be opened automatically. Switch back to an existing mail window, or restore the built-in frontend from Repair Options.",
        error_repair_unconfirmed: "The background didn't confirm the built-in frontend repair. Refresh the status.",
        error_repair_offline_failed: "Couldn't finish the offline restore. Make sure every background has stopped and the installer is complete, and check free disk space. Don't retry automatically.",
        error_platform_dirs_unavailable: "Platform directories are unavailable.",
        error_background_busy: "The background is still busy or starting. Refresh in a moment.",
        error_background_config_unavailable: "The local background configuration is unavailable. Check that the complete app is installed.",
        error_start_failed: "Couldn't start the embedded background.",
        error_stop_unconfirmed: "The background didn't confirm the stop. Refresh the status.",
        error_login_isolated_register: "An isolated user directory is in use, so system login items can't be registered.",
        error_login_register_failed: "The system didn't finish registering the login item. Check the app's signature and approve it in System Settings.",
        error_login_isolated_unregister: "An isolated user directory is in use, so system login items can't be unregistered.",
        error_login_unregister_failed: "The system didn't finish unregistering the login item. Check it in System Settings.",
        error_agent_path_invalid: "The embedded background path is invalid.",
        error_agent_launch_failed: "The system couldn't start the embedded background. Check the app's signature and integrity.",
        error_agent_launch_unsupported: "Starting the background app is not available on this platform yet.",
        error_open_mail_window_failed: "Couldn't open the mail window.",
        error_in_place_unsupported: "Opening the mail window in place isn't supported on this platform.",
    },
};

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn otp_body(code: &str, account: &str, from: &str) -> String {
    format!("{code} ({account} · from {from})")
}

fn new_mail_burst_body(account: &str) -> String {
    format!("Several new messages arrived in {account}")
}

fn new_mail_masked_body(account: &str) -> String {
    format!("A new message arrived in {account}")
}

fn reply_attribution(date: &str, sender: &str) -> String {
    format!("On {date}, {sender} wrote:")
}

fn forward_header(sender: &str, date: &str, subject: &str, recipient: &str) -> String {
    format!(
        "---------- Forwarded message ----------\n\
         From: {sender}\nDate: {date}\nSubject: {subject}\nTo: {recipient}"
    )
}

fn attribution_date(date: &AttributionDate) -> String {
    let (sign, offset_hours, offset_minutes) = date.utc_offset();
    let weekday = WEEKDAYS[date.weekday_index()];
    // 月份越界时不 panic：退回数字月份。
    let month = usize::from(date.month)
        .checked_sub(1)
        .and_then(|index| MONTHS.get(index))
        .map_or_else(|| date.month.to_string(), |name| (*name).to_owned());
    format!(
        "{weekday}, {month} {}, {} at {:02}:{:02} (UTC{sign}{offset_hours:02}:{offset_minutes:02})",
        date.day, date.year, date.hour, date.minute,
    )
}

fn quit(product: &str) -> String {
    format!("Quit {product}")
}

fn window_title(product: &str) -> String {
    format!("{product} Repair & Background")
}

fn status_summary(background: &str, mailbox: &str, login: &str, note: &str) -> String {
    format!(
        "Background: {background}\nMail frontend: {mailbox}\nLaunch at login: {login}\n\n{note}"
    )
}

fn note_open_from_full_app(app_bundle: &str) -> String {
    format!(
        "Open the complete {app_bundle} to manage the background and restore the mail frontend."
    )
}

fn login_manage_from_app(app_bundle: &str) -> String {
    format!("Manage from {app_bundle}")
}
