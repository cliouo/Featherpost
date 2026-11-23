#[cfg(target_os = "macos")]
mod implementation {
    use objc2_foundation::NSString;
    use objc2_service_management::{SMAppService, SMAppServiceStatus};
    pub(super) fn status() -> &'static str {
        if !system_home() {
            return "隔离目录：不更改系统登录项";
        }
        let id =
            NSString::from_str(maild_brand::AGENT_BUNDLE_ID);
        let service = unsafe { SMAppService::loginItemServiceWithIdentifier(&id) };
        match unsafe { service.status() } {
            SMAppServiceStatus::Enabled => "已注册",
            SMAppServiceStatus::RequiresApproval => "等待系统批准",
            SMAppServiceStatus::NotRegistered => "未注册",
            _ => "不可用",
        }
    }
    pub(super) fn register() -> Result<(), &'static str> {
        if !system_home() {
            return Err("当前使用隔离用户目录，不能注册系统登录项");
        }
        let id =
            NSString::from_str(maild_brand::AGENT_BUNDLE_ID);
        unsafe { SMAppService::loginItemServiceWithIdentifier(&id).registerAndReturnError() }
            .map_err(|_| "系统未完成登录项注册，请检查应用签名并在系统设置中批准")
    }
    pub(super) fn unregister() -> Result<(), &'static str> {
        if !system_home() {
            return Err("当前使用隔离用户目录，不能撤销系统登录项");
        }
        let id =
            NSString::from_str(maild_brand::AGENT_BUNDLE_ID);
        unsafe { SMAppService::loginItemServiceWithIdentifier(&id).unregisterAndReturnError() }
            .map_err(|_| "系统未完成登录项撤销，请在系统设置中检查")
    }
    pub(super) fn open_settings() {
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    }
    pub(super) fn login_enabled() -> bool {
        if !system_home() {
            return false;
        }
        let id =
            NSString::from_str(maild_brand::AGENT_BUNDLE_ID);
        let service = unsafe { SMAppService::loginItemServiceWithIdentifier(&id) };
        let status = unsafe { service.status() };
        status == SMAppServiceStatus::Enabled
    }
    /// Unsigned local Preview identity (`dev.<slug>.desktop`, any `dev.` bundle id): no login
    /// item, the launcher starts the Agent on open. Signed `com.`/`app.` builds use the login item.
    pub(super) fn uses_preview_agent() -> bool {
        maild_brand::APP_BUNDLE_ID.starts_with("dev.")
    }

    // ServiceManagement launches in the OS user's context, not a caller-supplied HOME. Keep
    // isolated bundle trials from accidentally registering a real-user background service.
    fn system_home() -> bool {
        maild_client::system_home()
    }
    /// ADR-0044: hand the embedded Agent app to LaunchServices only in the real user session.
    pub(super) fn launches_through_workspace() -> bool {
        system_home()
    }
    /// Open the embedded Agent app (`maild_brand::AGENT_BUNDLE_NAME`) as an independent background app (`activates = false`, no new
    /// instance). Waits briefly for LaunchServices' answer; an unanswered request is left to the
    /// caller's bounded online wait rather than reported as a failure.
    pub(super) fn open_agent_app(app: &std::path::Path) -> Result<(), &'static str> {
        use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
        use objc2_foundation::{NSError, NSURL};
        let text = app.to_str().ok_or("内嵌后台路径无效")?;
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(text), true);
        let configuration = NSWorkspaceOpenConfiguration::configuration();
        configuration.setActivates(false);
        configuration.setCreatesNewApplicationInstance(false);
        let (sender, receiver) = std::sync::mpsc::sync_channel::<bool>(1);
        let completion = block2::RcBlock::new(
            move |_app: *mut NSRunningApplication, error: *mut NSError| {
                let _ = sender.try_send(error.is_null());
            },
        );
        NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(
            &url,
            &configuration,
            Some(&completion),
        );
        match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(false) => Err("系统未能启动内嵌后台，请检查应用签名与完整性"),
            Ok(true) | Err(_) => Ok(()),
        }
    }
}
#[cfg(not(target_os = "macos"))]
mod implementation {
    pub(super) fn status() -> &'static str {
        "当前平台暂不提供登录项管理"
    }
    pub(super) fn register() -> Result<(), &'static str> {
        Err("当前平台暂不提供登录项管理")
    }
    pub(super) fn unregister() -> Result<(), &'static str> {
        Err("当前平台暂不提供登录项管理")
    }
    pub(super) fn open_settings() {}
    pub(super) fn login_enabled() -> bool {
        false
    }
    pub(super) fn uses_preview_agent() -> bool {
        false
    }
    pub(super) fn launches_through_workspace() -> bool {
        false
    }
    pub(super) fn open_agent_app(_app: &std::path::Path) -> Result<(), &'static str> {
        Err("当前平台暂不提供后台应用启动")
    }
}
pub(crate) fn status() -> &'static str {
    implementation::status()
}
pub(crate) fn register() -> Result<(), &'static str> {
    implementation::register()
}
pub(crate) fn unregister() -> Result<(), &'static str> {
    implementation::unregister()
}
pub(crate) fn open_settings() {
    implementation::open_settings()
}

/// Initialise AppKit only when a native window is actually shown (ADR-0044): a normal open that
/// hosts or hands off the mail window never becomes a foreground app of its own. Beside a
/// running main mail window (e.g. "Open recovery" from its settings), the recovery dialogs float
/// in front as an `Accessory` app, so no second Dock icon appears. Either way the app activates:
/// the menu bar spawns it as a plain child process, which macOS leaves behind the frontmost app.
pub(crate) fn prepare_ui() {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSApplication;
        use objc2_foundation::MainThreadMarker;
        static LAUNCHED: std::sync::Once = std::sync::Once::new();
        let marker = MainThreadMarker::new().expect("native recovery must run on the main thread");
        LAUNCHED.call_once(|| {
            #[allow(deprecated)]
            let _ = objc2_app_kit::NSApplicationLoad();
            let application = NSApplication::sharedApplication(marker);
            let beside_main_window = main_window_running();
            application.setActivationPolicy(activation_policy(beside_main_window));
            // Establish the standalone app lifecycle before running its native modal loops.
            application.finishLaunching();
            #[allow(deprecated)]
            application.activateIgnoringOtherApps(true);
        });
    }
}

#[cfg(target_os = "macos")]
fn activation_policy(beside_main_window: bool) -> objc2_app_kit::NSApplicationActivationPolicy {
    if beside_main_window {
        objc2_app_kit::NSApplicationActivationPolicy::Accessory
    } else {
        objc2_app_kit::NSApplicationActivationPolicy::Regular
    }
}

/// `true` while another process holds `runtime_dir/.main-ui.lock`.
#[cfg(target_os = "macos")]
fn main_window_running() -> bool {
    main_window_pid().is_some()
}

/// `Some(pid)` while a main mail window holds its lock (`pid` unreadable → `Some(None)`).
pub(crate) fn main_window_pid() -> Option<Option<i32>> {
    let paths = maild_paths::AppPaths::discover().ok()?;
    maild_client::running_main_ui(&paths.runtime_dir)
}

#[cfg(target_os = "macos")]
pub(crate) fn dialog(
    title: &str,
    body: &str,
    level: rfd::MessageLevel,
    labels: &[&str],
) -> Option<usize> {
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle};
    use objc2_foundation::{MainThreadMarker, NSString};
    let marker = MainThreadMarker::new()?;
    let alert = NSAlert::new(marker);
    let style = match level {
        rfd::MessageLevel::Info => NSAlertStyle::Informational,
        rfd::MessageLevel::Warning => NSAlertStyle::Warning,
        rfd::MessageLevel::Error => NSAlertStyle::Critical,
    };
    alert.setAlertStyle(style);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(body));
    for label in labels {
        alert.addButtonWithTitle(&NSString::from_str(label));
    }
    let response = alert.runModal();
    usize::try_from(response - NSAlertFirstButtonReturn)
        .ok()
        .filter(|index| *index < labels.len())
}

#[cfg(target_os = "macos")]
pub(crate) fn focus(pid: i32) {
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        let _ = app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn focus(_pid: i32) {}
pub(crate) fn login_enabled() -> bool {
    implementation::login_enabled()
}
pub(crate) fn uses_preview_agent() -> bool {
    implementation::uses_preview_agent()
}
pub(crate) fn launches_through_workspace() -> bool {
    implementation::launches_through_workspace()
}
pub(crate) fn open_agent_app(app: &std::path::Path) -> Result<(), &'static str> {
    implementation::open_agent_app(app)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn recovery_beside_the_main_window_never_adds_a_dock_icon() {
        use objc2_app_kit::NSApplicationActivationPolicy;
        assert_eq!(
            super::activation_policy(true),
            NSApplicationActivationPolicy::Accessory
        );
        assert_eq!(
            super::activation_policy(false),
            NSApplicationActivationPolicy::Regular
        );
    }
}
