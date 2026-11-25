//! Top-level app bundle executable (`maild_brand::APP_BUNDLE_NAME`) (ADR-0039, ADR-0044). No mail rendering, egui, webview, or
//! frontend-package dependency: it prepares platform config and ensures a background. When the
//! bundled frontend is the healthy default it becomes that window in place (`exec` of the
//! same-bundle `Contents/MacOS/<UI_BIN>`, keeping PID and the single product Dock identity);
//! a third-party default is handed to the Agent; otherwise it shows the native recovery window.
//! AppKit is only initialised when a native dialog is actually shown.
mod bootstrap;
mod l10n;
mod platform;
mod runtime;
mod ui;
mod window_owner;

pub use runtime::{Controller, LaunchOutcome, NativeController, Operation, Status};
pub use ui::show_recovery;

#[used]
static AGENT_ID_CONTRACT: &str = maild_brand::agent_id_contract!();

/// First-run preinstall of the bundled frontend happens inside the Agent under its directory
/// lease, so it also covers login-item starts and upgrades without this process.
pub fn run(repair: bool) {
    // No `prepare_ui()` here (ADR-0044): a normal open that hosts or hands off the mail window
    // must never register a second foreground app. Native dialogs initialise AppKit lazily.
    // Before runtime/IPC/dialog creation: duplicate opens only focus the existing owner.
    let installed = !maild_client::NativeAppLayout::has_custom_connection()
        && maild_client::NativeAppLayout::discover().is_ok_and(|layout| layout.is_some());
    let mut window_owner = if installed {
        match acquire_window() {
            Some(owner) => Some(owner),
            None => return,
        }
    } else {
        None
    };
    if installed && !repair {
        if let Err(message) = bootstrap::prepare_installed_config() {
            eprintln!("{}: {message}", maild_brand::LAUNCHER_BIN);
        }
    }
    match NativeController::discover() {
        Ok(mut controller) => {
            if !repair {
                // Keep ownership while starting/awaiting the Agent, but release it right before
                // the activation request: an asynchronous activation failure makes the Agent
                // start `--repair`, which must be able to own the window after this exits.
                let mut released = false;
                match controller.open_on_launch(|| released = window_owner.take().is_some()) {
                    LaunchOutcome::Requested | LaunchOutcome::Focused => return,
                    LaunchOutcome::InPlace(exe) => {
                        // The mail window may later open `--repair` itself; never hold the lock.
                        released |= window_owner.take().is_some();
                        drop(controller);
                        match host_in_place(&exe) {
                            Ok(()) => return,
                            Err(message) => {
                                eprintln!("{}: {message}", maild_brand::LAUNCHER_BIN);
                                controller = match NativeController::discover() {
                                    Ok(controller) => controller,
                                    Err(message) => return ui::show_error(message),
                                };
                            }
                        }
                    }
                    LaunchOutcome::Recovery => {}
                }
                if released {
                    match acquire_window() {
                        Some(owner) => window_owner = Some(owner),
                        // A repair window opened meanwhile already shows the honest status.
                        None => return,
                    }
                }
            }
            let mailbox_host = show_recovery(&mut controller);
            drop(controller);
            #[cfg_attr(not(target_os = "macos"), allow(clippy::drop_non_drop))]
            drop(window_owner);
            if let Some(exe) = mailbox_host {
                if let Err(message) = host_in_place(&exe) {
                    ui::show_error(message);
                }
            }
        }
        Err(message) => {
            ui::show_error(message);
        }
    }
}

/// Become the bundled mail window (ADR-0044). A running window (holder of `.main-ui.lock`) is
/// normally already woken by `open_on_launch` (ADR-0045); this re-check only covers the race
/// where it appeared in between, and brings it forward: zero new processes. Otherwise this process `exec`s the same-bundle
/// UI executable with the sanitised native child environment, keeping its PID and LaunchServices
/// identity, so the one product Dock icon the user clicked lights up. No redundant inbox intent
/// is queued: it would supersede a notification/menu intent, and the window defaults to the inbox.
/// Returns only on failure.
fn host_in_place(exe: &std::path::Path) -> Result<(), &'static str> {
    if let Some(pid) = platform::main_window_pid_for(maild::recovery::BUNDLED_FRONTEND_ID) {
        if let Some(pid) = pid {
            platform::focus(pid);
        }
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let _error = maild_client::native_child_command(exe).exec();
        Err(l10n::text().error_open_mail_window_failed)
    }
    #[cfg(not(unix))]
    {
        let _ = exe;
        Err(l10n::text().error_in_place_unsupported)
    }
}

/// `None` when another window owns the UI (it was asked to come forward) or ownership is unsafe.
fn acquire_window() -> Option<window_owner::WindowOwner> {
    match window_owner::acquire() {
        Ok(owner) => owner,
        Err(message) => {
            // No blocking dialog without ownership: repeated failed opens must not leave an
            // unbounded set of windows/processes behind an unsafe runtime directory.
            eprintln!("{}: {message}", maild_brand::LAUNCHER_BIN);
            None
        }
    }
}
