//! Single main mail window per platform runtime (ADR-0044).
//!
//! The main app bundle (`maild_brand::APP_BUNDLE_NAME`) hosts this executable in place, notifications and the menu bar re-open that same
//! app, and Finder/`open -n` can still start a second copy. Whoever starts second only brings
//! the first window forward and exits, so there is never a second product Dock icon or a second
//! connection claiming the same one-shot intents. The lock lives in `runtime_dir/.main-ui.lock`
//! (`0600`, `O_NOFOLLOW | O_CLOEXEC`, holder PID) and is released when the process exits.
use std::path::Path;

pub use maild_client::MainUiLock;

/// Startup decision for the main window process.
pub enum InstanceClaim {
    /// Continue starting; keep the guard (if any) alive until `main` returns.
    Owner(Option<MainUiLock>),
    /// Another window owns the runtime and was asked to come forward; exit successfully.
    Focused,
}

/// Claim the platform runtime's main window. Custom connections (`--config/--socket/--pipe/--url`
/// or their environment variables) are development/test contexts and never take the lock; any
/// lock failure (e.g. the runtime directory is not prepared yet) degrades to an unlocked window
/// rather than refusing to show mail.
pub fn claim(custom_connection: bool) -> InstanceClaim {
    if custom_connection || maild_client::NativeAppLayout::has_custom_connection() {
        return InstanceClaim::Owner(None);
    }
    let Ok(paths) = maild_paths::AppPaths::discover() else {
        return InstanceClaim::Owner(None);
    };
    // Both legacy UI and Studio use the unified platform runtime `.main-ui.lock` (ADR-0044)
    // so launcher minimized-window wake, `--repair` Accessory policy, menu-bar offline cleanup,
    // and duplicate standalone-bundle launches all converge on the single main window.
    match claim_at(&paths.runtime_dir, focus) {
        Ok(Some(lock)) => InstanceClaim::Owner(Some(lock)),
        Ok(None) => InstanceClaim::Focused,
        Err(message) => {
            eprintln!("maild-ui: {message}，继续以无单实例锁方式启动");
            InstanceClaim::Owner(None)
        }
    }
}

/// `Ok(None)`: another live process holds the lock; `focus` received its PID when readable.
pub fn claim_at(
    runtime_dir: &Path,
    focus: impl FnOnce(i32),
) -> Result<Option<MainUiLock>, &'static str> {
    match maild_client::claim_main_ui(runtime_dir)? {
        maild_client::MainUiClaim::Acquired(lock) => Ok(Some(lock)),
        maild_client::MainUiClaim::Held { pid } => {
            if let Some(pid) = pid.filter(|pid| *pid != std::process::id() as i32) {
                focus(pid);
            }
            Ok(None)
        }
    }
}

#[cfg(target_os = "macos")]
fn focus(pid: i32) {
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        let _ = app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    }
}

#[cfg(not(target_os = "macos"))]
fn focus(_pid: i32) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn runtime() -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("run");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        (root, runtime)
    }

    #[test]
    fn second_instance_focuses_the_first_and_release_allows_reopen() {
        let (_root, runtime) = runtime();
        let first = claim_at(&runtime, |_| panic!("first window focuses nobody"))
            .unwrap()
            .expect("first window owns the lock");
        let pid = std::fs::read_to_string(runtime.join(maild_client::MAIN_UI_LOCK)).unwrap();
        assert_eq!(pid, std::process::id().to_string());

        // A second window in another process would focus this PID; the same PID is never
        // "focused" from itself, but it still must not start a second window.
        assert!(claim_at(&runtime, |_| panic!("own PID is never focused"))
            .unwrap()
            .is_none());
        std::fs::write(runtime.join(maild_client::MAIN_UI_LOCK), "1").unwrap();
        let focused = std::cell::Cell::new(None);
        assert!(claim_at(&runtime, |pid| focused.set(Some(pid)))
            .unwrap()
            .is_none());
        assert_eq!(focused.get(), Some(1), "the recorded owner is brought forward");

        drop(first);
        let _again = claim_at(&runtime, |_| panic!("released lock cannot block reopen"))
            .unwrap()
            .expect("reopen after the first window exits");
    }

    #[test]
    fn unsafe_lock_never_focuses_or_follows_links() {
        let (root, runtime) = runtime();
        std::os::unix::fs::symlink(
            root.path().join("outside"),
            runtime.join(maild_client::MAIN_UI_LOCK),
        )
        .unwrap();
        assert!(claim_at(&runtime, |_| panic!("unsafe lock cannot focus")).is_err());
        assert!(!root.path().join("outside").exists());
    }

    #[test]
    fn custom_connections_never_take_the_platform_lock() {
        assert!(matches!(claim(true), InstanceClaim::Owner(None)));
    }
}
