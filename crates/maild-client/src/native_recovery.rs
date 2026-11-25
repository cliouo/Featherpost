//! Fixed native recovery entry points. Mail identities never become argv or environment values.
//!
//! ADR-0044: the same exact top-level app layout (`maild_brand::APP_BUNDLE_NAME`) also locates
//! the in-place UI executable (`Contents/MacOS/<UI_BIN>`) and the embedded Agent app, and owns
//! the `.main-ui.lock` single-instance contract shared by the UI (holder) and the launcher
//! (probe). Every bundle and executable name comes from `maild_brand`.
use std::path::{Path, PathBuf};

use maild_brand::{
    AGENT_BIN, AGENT_BUNDLE_NAME, APP_BUNDLE_NAME, BUNDLED_FRONTEND_ID, LAUNCHER_BIN, MENU_BIN,
    MENU_BUNDLE_NAME, UI_BIN,
};

fn launcher() -> PathBuf {
    Path::new("Contents/MacOS").join(LAUNCHER_BIN)
}
/// ADR-0044: the bundled frontend executable hosted in place by the top-level app bundle.
fn ui() -> PathBuf {
    Path::new("Contents/MacOS").join(UI_BIN)
}
/// ADR-0044: the bundled Studio executable hosted in place by the top-level app bundle.
fn studio_ui() -> PathBuf {
    Path::new("Contents/MacOS").join(crate::STUDIO_BIN)
}
fn agent_app() -> PathBuf {
    Path::new("Contents/Library/LoginItems").join(AGENT_BUNDLE_NAME)
}
fn agent() -> PathBuf {
    agent_app().join("Contents/MacOS").join(AGENT_BIN)
}
fn menu() -> PathBuf {
    Path::new("Contents/Library/Helpers")
        .join(MENU_BUNDLE_NAME)
        .join("Contents/MacOS")
        .join(MENU_BIN)
}
/// Runtime-directory file locked by the single running main mail window (ADR-0044).
pub const MAIN_UI_LOCK: &str = ".main-ui.lock";

pub struct NativeAppLayout {
    app: PathBuf,
}

/// Preserve the platform path context while discarding API credentials/interactive overrides.
/// An isolated HOME must never become the real home in a recovery or Agent child.
pub fn native_child_command(executable: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(executable);
    command.env_clear().current_dir("/");
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_RUNTIME_DIR",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "TMPDIR",
        "TEMP",
        "TMP",
        "LANG",
        "LC_ALL",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

impl NativeAppLayout {
    pub fn has_custom_connection() -> bool {
        [
            maild_paths::CONFIG_ENV,
            crate::API_URL_ENV,
            crate::API_TOKEN_ENV,
            crate::API_SOCKET_ENV,
            crate::API_PIPE_ENV,
        ]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
    }
    pub fn discover() -> Result<Option<Self>, &'static str> {
        let exe = std::env::current_exe().map_err(|_| "无法确定应用位置")?;
        Self::from_executable(&exe)
    }

    fn from_executable(exe: &Path) -> Result<Option<Self>, &'static str> {
        let suffixes = [launcher(), ui(), studio_ui(), agent(), menu()];
        let app = suffixes.iter().find_map(|suffix| {
            let root = exe.ancestors().nth(suffix.components().count())?;
            (root.file_name()? == APP_BUNDLE_NAME && root.join(suffix) == exe).then_some(root)
        });
        let Some(app) = app else {
            return Ok(None);
        };
        checked_executable(app, exe)?;
        Ok(Some(Self { app: app.into() }))
    }

    /// The top-level app bundle (`maild_brand::APP_BUNDLE_NAME`) (the single Dock identity, ADR-0044).
    pub fn app_bundle(&self) -> &Path {
        &self.app
    }

    pub fn agent_executable(&self) -> Result<PathBuf, &'static str> {
        let exe = self.app.join(agent());
        checked_executable(&self.app, &exe)?;
        Ok(exe)
    }

    /// The embedded Agent app (`maild_brand::AGENT_BUNDLE_NAME`), opened through LaunchServices so the background runs as
    /// its own `LSUIElement` service outside the launcher's coalition (ADR-0044). Only returned
    /// when its executable passes the same exact-chain checks as a direct spawn.
    pub fn agent_app_bundle(&self) -> Result<PathBuf, &'static str> {
        self.agent_executable()?;
        Ok(self.app.join(agent_app()))
    }

    /// `Contents/MacOS/<UI_BIN>`, hosted in place by the top-level app (ADR-0044). Rejects symlinks,
    /// foreign owners, group/other-writable components and non-executable files.
    pub fn ui_executable(&self) -> Result<PathBuf, &'static str> {
        let exe = self.app.join(ui());
        checked_executable(&self.app, &exe)?;
        Ok(exe)
    }

    /// `Contents/MacOS/<STUDIO_BIN>`, hosted in place by the top-level app when Studio is bundled.
    /// Rejects symlinks, foreign owners, group/other-writable components and non-executable files.
    pub fn studio_ui_executable(&self) -> Result<PathBuf, &'static str> {
        let exe = self.app.join(studio_ui());
        checked_executable(&self.app, &exe)?;
        Ok(exe)
    }

    /// In-place UI executable for a bundled frontend id (`BUNDLED_FRONTEND_ID` or
    /// `STUDIO_FRONTEND_ID`). Returns `Ok(None)` for third-party frontend ids.
    pub fn frontend_ui_executable(&self, id: &str) -> Result<Option<PathBuf>, &'static str> {
        if id == BUNDLED_FRONTEND_ID {
            self.ui_executable().map(Some)
        } else if id == crate::STUDIO_FRONTEND_ID {
            self.studio_ui_executable().map(Some)
        } else {
            Ok(None)
        }
    }

    /// Packaged recovery payload for a bundled frontend id (unchecked path; callers inspect it).
    pub fn bundled_frontend_payload_for(&self, id: &str) -> PathBuf {
        self.app.join("Contents/Resources/frontends").join(id)
    }

    /// Packaged recovery payload of the bundled frontend (unchecked path; callers inspect it).
    pub fn bundled_frontend_payload(&self) -> PathBuf {
        self.bundled_frontend_payload_for(BUNDLED_FRONTEND_ID)
    }

    /// Packaged recovery payload of the bundled Studio frontend (unchecked path; callers inspect it).
    pub fn studio_frontend_payload(&self) -> PathBuf {
        self.bundled_frontend_payload_for(crate::STUDIO_FRONTEND_ID)
    }

    fn recovery_executable(&self) -> Result<PathBuf, &'static str> {
        let exe = self.app.join(launcher());
        checked_executable(&self.app, &exe)?;
        Ok(exe)
    }
}

/// `true` only when `HOME` is the OS user's real home (or unset). LaunchServices and
/// ServiceManagement start processes in the OS user's context, not a caller-supplied `HOME`, so
/// isolated trials (e.g. a `/private/tmp` HOME) must keep direct, environment-preserving children.
#[cfg(unix)]
pub fn system_home() -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Some(home) = std::env::var_os("HOME") else {
        return true;
    };
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut found = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 64 * 1024];
    let code = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    };
    if code != 0 || found.is_null() {
        return false;
    }
    let entry = unsafe { entry.assume_init() };
    if entry.pw_dir.is_null() {
        return false;
    }
    let real = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    let real = Path::new(std::ffi::OsStr::from_bytes(real.to_bytes()));
    match (Path::new(&home).canonicalize(), real.canonicalize()) {
        (Ok(home), Ok(real)) => home == real,
        _ => false,
    }
}

/// Without a POSIX user database there is no platform session to hand processes to.
#[cfg(not(unix))]
pub fn system_home() -> bool {
    false
}

/// Held for the lifetime of the single main mail window process (ADR-0044).
pub struct MainUiLock {
    #[cfg(unix)]
    _file: std::fs::File,
}

/// Outcome of claiming the main mail window.
pub enum MainUiClaim {
    /// This process owns the main window until the guard drops (or the process exits/execs).
    Acquired(MainUiLock),
    /// Another live process owns it; `pid` is its recorded PID when readable.
    Held { pid: Option<i32> },
}

/// Non-blocking claim of `runtime_dir/.main-ui.lock` (`0600`, `O_NOFOLLOW | O_CLOEXEC`, PID
/// recorded). A missing runtime directory is an error: the window never creates platform
/// directories, which the Agent prepares under its lifecycle lock.
#[cfg(unix)]
pub fn claim_main_ui(runtime_dir: &Path) -> Result<MainUiClaim, &'static str> {
    use std::io::Write as _;
    let mut file = open_main_ui_lock(runtime_dir, true)?.ok_or("主窗口锁不可用")?;
    // A launcher probe holds a shared lock for microseconds; do not mistake it for a window.
    for attempt in 0..5 {
        match file.try_lock() {
            Ok(()) => {
                file.set_len(0).map_err(|_| "主窗口锁不可写")?;
                write!(file, "{}", std::process::id()).map_err(|_| "主窗口锁不可写")?;
                file.flush().map_err(|_| "主窗口锁不可写")?;
                return Ok(MainUiClaim::Acquired(MainUiLock { _file: file }));
            }
            Err(std::fs::TryLockError::WouldBlock) if attempt < 4 => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(std::fs::TryLockError::WouldBlock) => break,
            Err(_) => return Err("主窗口锁不可用"),
        }
    }
    Ok(MainUiClaim::Held {
        pid: read_lock_pid(&mut file),
    })
}

#[cfg(not(unix))]
pub fn claim_main_ui(_runtime_dir: &Path) -> Result<MainUiClaim, &'static str> {
    Ok(MainUiClaim::Acquired(MainUiLock {}))
}

/// Read-only probe: `Some(pid)` when another process holds the main window lock (`pid` is
/// `None` when unreadable). Never creates the lock file; any unsafe/missing state is "absent".
#[cfg(unix)]
pub fn running_main_ui(runtime_dir: &Path) -> Option<Option<i32>> {
    let mut file = open_main_ui_lock(runtime_dir, false).ok()??;
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            None
        }
        Err(std::fs::TryLockError::WouldBlock) => Some(read_lock_pid(&mut file)),
        Err(_) => None,
    }
}

#[cfg(not(unix))]
pub fn running_main_ui(_runtime_dir: &Path) -> Option<Option<i32>> {
    None
}

#[cfg(unix)]
fn open_main_ui_lock(
    runtime_dir: &Path,
    create: bool,
) -> Result<Option<std::fs::File>, &'static str> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let uid = unsafe { libc::geteuid() };
    let directory = std::fs::symlink_metadata(runtime_dir).map_err(|_| "运行目录不可用")?;
    if directory.file_type().is_symlink()
        || !directory.is_dir()
        || directory.uid() != uid
        || directory.mode() & 0o022 != 0
    {
        return Err("运行目录类型或权限无效");
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(runtime_dir.join(MAIN_UI_LOCK))
    {
        Ok(file) => file,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("主窗口锁不可用"),
    };
    let meta = file.metadata().map_err(|_| "主窗口锁不可检查")?;
    if !meta.is_file() || meta.uid() != uid || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
        return Err("主窗口锁类型或权限无效");
    }
    Ok(Some(file))
}

#[cfg(unix)]
fn read_lock_pid(file: &mut std::fs::File) -> Option<i32> {
    use std::io::{Read as _, Seek as _};
    let mut pid = String::new();
    file.rewind().ok()?;
    file.take(16).read_to_string(&mut pid).ok()?;
    pid.trim().parse::<i32>().ok().filter(|pid| *pid > 0)
}

pub fn open_native_recovery() -> Result<(), &'static str> {
    static LAST_OPEN: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST_OPEN.lock().map_err(|_| "修复入口状态不可用")?;
    if NativeAppLayout::has_custom_connection() {
        return Err("自定义连接不能启动平台修复入口");
    }
    let layout = NativeAppLayout::discover()?.ok_or("当前进程不在完整应用包中")?;
    let exe = layout.recovery_executable()?;
    if last.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(1)) {
        return Ok(());
    }
    let mut child = native_child_command(&exe)
        .arg("--repair")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "无法打开原生修复窗口")?;
    *last = Some(std::time::Instant::now());
    // A GUI may stay open indefinitely. Reap it without blocking the API or notification loop.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn checked_executable(root: &Path, path: &Path) -> Result<(), &'static str> {
    let relative = path.strip_prefix(root).map_err(|_| "程序不在应用内")?;
    let mut current = root.to_path_buf();
    let mut paths = vec![current.clone()];
    for part in relative.components() {
        if !matches!(part, std::path::Component::Normal(_)) {
            return Err("应用路径无效");
        }
        current.push(part);
        paths.push(current.clone());
    }
    for current in paths {
        let metadata = std::fs::symlink_metadata(&current).map_err(|_| "应用不完整")?;
        if metadata.file_type().is_symlink()
            || if current == path {
                !metadata.is_file()
            } else {
                !metadata.is_dir()
            }
        {
            return Err("应用路径类型无效");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o022 != 0
                || (current == path && metadata.mode() & 0o100 == 0)
            {
                return Err("应用所有者或执行权限无效");
            }
        }
    }
    Ok(())
}

/// Best-effort read-only inspection of the configured default frontend on disk (persisted
/// `frontends/.default.json` first, then `plugins.frontend.config.default` in `config_file`).
/// Returns `None` whenever the selection cannot be safely determined (missing, corrupt, unsafe
/// permissions, or symlink). Never creates or modifies any file.
pub fn configured_default_frontend(paths: &maild_paths::AppPaths) -> Option<String> {
    match read_persisted_default(&paths.frontend_dir.join(".default.json")) {
        PersistedDefault::Found(id) => return Some(id),
        PersistedDefault::Unreadable => return None,
        PersistedDefault::Missing => {}
    }
    read_config_default(&paths.config_file)
}

enum PersistedDefault {
    Found(String),
    Missing,
    Unreadable,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedDefaultFile {
    version: u32,
    default: String,
}

fn valid_frontend_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !value.is_empty()
        && value.len() <= 128
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn migrate_legacy_default(id: &str) -> String {
    if id == crate::LEGACY_EGUI_FRONTEND {
        crate::DEFAULT_FRONTEND.to_owned()
    } else {
        id.to_owned()
    }
}

fn read_persisted_default(path: &Path) -> PersistedDefault {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return PersistedDefault::Missing;
        }
        Err(_) => return PersistedDefault::Unreadable,
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return PersistedDefault::Unreadable;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return PersistedDefault::Unreadable;
        }
    }
    let Ok(bytes) = std::fs::read(path) else {
        return PersistedDefault::Unreadable;
    };
    let Ok(state) = serde_json::from_slice::<PersistedDefaultFile>(&bytes) else {
        return PersistedDefault::Unreadable;
    };
    if state.version != 1 || !valid_frontend_id(&state.default) {
        return PersistedDefault::Unreadable;
    }
    PersistedDefault::Found(migrate_legacy_default(&state.default))
}

fn read_config_default(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > crate::MAX_CONFIG_BYTES as u64
    {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return None;
        }
    }
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: toml::Value = toml::from_str(&text).ok()?;
    let default = parsed
        .get("plugins")?
        .get("frontend")?
        .get("config")?
        .get("default")?
        .as_str()?;
    valid_frontend_id(default).then(|| migrate_legacy_default(default))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_exe(app: &Path) -> PathBuf {
        app.join("Contents/Library/LoginItems")
            .join(AGENT_BUNDLE_NAME)
            .join("Contents/MacOS")
            .join(AGENT_BIN)
    }
    fn macos_exe(app: &Path, name: &str) -> PathBuf {
        app.join("Contents/MacOS").join(name)
    }
    #[test]
    fn child_context_preserves_platform_paths_without_api_secrets() {
        let command = native_child_command(Path::new("/not-executed"));
        let values: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        for name in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            let expected = std::env::var_os(name);
            assert!(
                values.get(std::ffi::OsStr::new(name)).copied().flatten() == expected.as_deref()
            );
        }
        assert!(!values.contains_key(std::ffi::OsStr::new(crate::API_TOKEN_ENV)));
        assert!(!values.contains_key(std::ffi::OsStr::new(maild_paths::CONFIG_ENV)));
    }
    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"fixture, never executed").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    #[test]
    fn exact_layout_resolves_the_launcher_from_every_nested_executable() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join(APP_BUNDLE_NAME);
        let agent = agent_exe(&app);
        executable(&agent);
        let layout = NativeAppLayout::from_executable(&agent).unwrap().unwrap();
        assert!(layout.recovery_executable().is_err(), "no launcher, no fallback");
        assert_eq!(layout.agent_executable().unwrap(), agent);
        let launcher = macos_exe(&app, LAUNCHER_BIN);
        executable(&launcher);
        assert_eq!(layout.recovery_executable().unwrap(), launcher);
        // The removed egui executable is not an entry point any more.
        let old = macos_exe(&app, "legacy-desktop");
        executable(&old);
        assert!(NativeAppLayout::from_executable(&old).unwrap().is_none());
        assert!(
            NativeAppLayout::from_executable(&app.join("elsewhere").join(AGENT_BIN))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn menu_executable_resolves_the_top_level_app() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join(APP_BUNDLE_NAME);
        let menu = app
            .join("Contents/Library/Helpers")
            .join(MENU_BUNDLE_NAME)
            .join("Contents/MacOS")
            .join(MENU_BIN);
        executable(&menu);
        let layout = NativeAppLayout::from_executable(&menu).unwrap().unwrap();
        assert_eq!(layout.app_bundle(), app);
        let renamed = root.path().join("Other.app").join("Contents/MacOS").join(LAUNCHER_BIN);
        executable(&renamed);
        assert!(NativeAppLayout::from_executable(&renamed).unwrap().is_none());
    }
    #[cfg(unix)]
    #[test]
    fn substituted_launcher_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join(APP_BUNDLE_NAME);
        let agent = agent_exe(&app);
        executable(&agent);
        let layout = NativeAppLayout::from_executable(&agent).unwrap().unwrap();
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::os::unix::fs::symlink(&agent, macos_exe(&app, LAUNCHER_BIN)).unwrap();
        assert!(layout.recovery_executable().is_err());
    }
    #[test]
    fn in_place_ui_resolves_the_app_and_its_agent_bundle() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join(APP_BUNDLE_NAME);
        let ui = macos_exe(&app, UI_BIN);
        executable(&ui);
        let layout = NativeAppLayout::from_executable(&ui).unwrap().unwrap();
        assert_eq!(layout.app_bundle(), app);
        assert_eq!(layout.ui_executable().unwrap(), ui);
        assert!(layout.agent_app_bundle().is_err(), "no agent, no bundle");
        let agent = agent_exe(&app);
        executable(&agent);
        assert_eq!(
            layout.agent_app_bundle().unwrap(),
            app.join("Contents/Library/LoginItems").join(AGENT_BUNDLE_NAME)
        );
        assert_eq!(
            layout.bundled_frontend_payload(),
            app.join("Contents/Resources/frontends").join(BUNDLED_FRONTEND_ID)
        );
        // The installed frontend copy elsewhere is never mistaken for the top-level app.
        let installed = root
            .path()
            .join("frontends")
            .join(BUNDLED_FRONTEND_ID)
            .join(maild_brand::UI_BUNDLE_NAME)
            .join("Contents/MacOS")
            .join(UI_BIN);
        executable(&installed);
        assert!(NativeAppLayout::from_executable(&installed).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn in_place_ui_and_agent_bundle_reject_symlinks_and_wide_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join(APP_BUNDLE_NAME);
        let launcher = macos_exe(&app, LAUNCHER_BIN);
        executable(&launcher);
        let layout = NativeAppLayout::from_executable(&launcher).unwrap().unwrap();
        // A symlinked UI executable is never exec'd in place.
        let elsewhere = root.path().join(UI_BIN);
        executable(&elsewhere);
        std::os::unix::fs::symlink(&elsewhere, macos_exe(&app, UI_BIN)).unwrap();
        assert!(layout.ui_executable().is_err());
        std::fs::remove_file(macos_exe(&app, UI_BIN)).unwrap();
        let ui = macos_exe(&app, UI_BIN);
        executable(&ui);
        assert!(layout.ui_executable().is_ok());
        std::fs::set_permissions(&ui, std::fs::Permissions::from_mode(0o722)).unwrap();
        assert!(layout.ui_executable().is_err(), "group/other-writable UI");
        std::fs::set_permissions(&ui, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(layout.ui_executable().is_err(), "non-executable UI");
        // A world-writable directory inside the agent chain invalidates the agent bundle.
        let agent = agent_exe(&app);
        executable(&agent);
        assert!(layout.agent_app_bundle().is_ok());
        let login_items = app.join("Contents/Library/LoginItems");
        std::fs::set_permissions(&login_items, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(layout.agent_app_bundle().is_err());
        std::fs::set_permissions(&login_items, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A symlinked agent app directory is rejected as well.
        let agent_app = app.join("Contents/Library/LoginItems").join(AGENT_BUNDLE_NAME);
        std::fs::rename(&agent_app, root.path().join("Agent.app")).unwrap();
        std::os::unix::fs::symlink(root.path().join("Agent.app"), &agent_app).unwrap();
        assert!(layout.agent_app_bundle().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn main_ui_lock_is_single_owner_and_reopens_after_release() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("run");
        assert!(claim_main_ui(&runtime).is_err(), "never creates the runtime dir");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(running_main_ui(&runtime).is_none(), "probe never creates the lock");
        assert!(!runtime.join(MAIN_UI_LOCK).exists());
        let MainUiClaim::Acquired(first) = claim_main_ui(&runtime).unwrap() else {
            panic!("first window owns the lock");
        };
        let mode = std::fs::metadata(runtime.join(MAIN_UI_LOCK)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let me = std::process::id() as i32;
        assert_eq!(running_main_ui(&runtime), Some(Some(me)));
        assert!(matches!(
            claim_main_ui(&runtime).unwrap(),
            MainUiClaim::Held { pid: Some(pid) } if pid == me
        ));
        drop(first);
        assert!(running_main_ui(&runtime).is_none());
        let MainUiClaim::Acquired(_again) = claim_main_ui(&runtime).unwrap() else {
            panic!("released lock reopens");
        };
    }

    #[cfg(unix)]
    #[test]
    fn main_ui_lock_rejects_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("run");
        std::fs::create_dir(&runtime).unwrap();
        std::os::unix::fs::symlink(root.path().join("outside"), runtime.join(MAIN_UI_LOCK))
            .unwrap();
        assert!(claim_main_ui(&runtime).is_err());
        assert!(running_main_ui(&runtime).is_none());
        assert!(!root.path().join("outside").exists());
    }

    #[test]
    fn in_place_studio_resolves_the_app_and_rejects_standalone_copy() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join(APP_BUNDLE_NAME);
        let studio = macos_exe(&app, crate::STUDIO_BIN);
        executable(&studio);
        let layout = NativeAppLayout::from_executable(&studio).unwrap().unwrap();
        assert_eq!(layout.app_bundle(), app);
        assert_eq!(layout.studio_ui_executable().unwrap(), studio);
        assert_eq!(
            layout
                .frontend_ui_executable(crate::STUDIO_FRONTEND_ID)
                .unwrap(),
            Some(studio.clone())
        );
        assert_eq!(
            layout
                .frontend_ui_executable("org.example.reader")
                .unwrap(),
            None
        );
        assert_eq!(
            layout.studio_frontend_payload(),
            app.join("Contents/Resources/frontends")
                .join(crate::STUDIO_FRONTEND_ID)
        );
        // Standalone Studio inside frontends/ is never mistaken for the top-level app.
        let standalone = root
            .path()
            .join("frontends")
            .join(crate::STUDIO_FRONTEND_ID)
            .join("Featherpost Studio.app/Contents/MacOS")
            .join(crate::STUDIO_BIN);
        executable(&standalone);
        assert!(NativeAppLayout::from_executable(&standalone)
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn in_place_studio_rejects_symlinks_and_wide_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join(APP_BUNDLE_NAME);
        let launcher = macos_exe(&app, LAUNCHER_BIN);
        executable(&launcher);
        let layout = NativeAppLayout::from_executable(&launcher).unwrap().unwrap();
        assert!(layout.studio_ui_executable().is_err());

        let elsewhere = root.path().join(crate::STUDIO_BIN);
        executable(&elsewhere);
        std::os::unix::fs::symlink(&elsewhere, macos_exe(&app, crate::STUDIO_BIN)).unwrap();
        assert!(layout.studio_ui_executable().is_err());
        std::fs::remove_file(macos_exe(&app, crate::STUDIO_BIN)).unwrap();

        let studio = macos_exe(&app, crate::STUDIO_BIN);
        executable(&studio);
        assert!(layout.studio_ui_executable().is_ok());
        std::fs::set_permissions(&studio, std::fs::Permissions::from_mode(0o722)).unwrap();
        assert!(layout.studio_ui_executable().is_err());
        std::fs::set_permissions(&studio, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(layout.studio_ui_executable().is_err());
    }

    #[test]
    fn configured_default_frontend_reads_state_then_config_and_fails_open_to_none_on_corruption() {
        let root = tempfile::tempdir().unwrap();
        let paths = maild_paths::AppPaths::from_roots(
            &root.path().join("config"),
            &root.path().join("data"),
            Some(&root.path().join("run")),
            None,
            root.path(),
        );
        assert_eq!(configured_default_frontend(&paths), None);

        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            &paths.config_file,
            format!(
                "[plugins.frontend.config]\ndefault = \"{}\"\n",
                crate::STUDIO_FRONTEND_ID
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&paths.config_file, std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        assert_eq!(
            configured_default_frontend(&paths).as_deref(),
            Some(crate::STUDIO_FRONTEND_ID)
        );

        // .default.json takes precedence over config.
        std::fs::create_dir_all(&paths.frontend_dir).unwrap();
        let state_path = paths.frontend_dir.join(".default.json");
        std::fs::write(
            &state_path,
            format!(r#"{{"version":1,"default":"{}"}}"#, BUNDLED_FRONTEND_ID),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            configured_default_frontend(&paths).as_deref(),
            Some(BUNDLED_FRONTEND_ID)
        );

        // Legacy builtin-egui migrates to DEFAULT_FRONTEND.
        std::fs::write(&state_path, r#"{"version":1,"default":"builtin-egui"}"#).unwrap();
        assert_eq!(
            configured_default_frontend(&paths).as_deref(),
            Some(crate::DEFAULT_FRONTEND)
        );

        // Corrupt .default.json returns None (never silently falls back to config).
        std::fs::write(&state_path, b"not json").unwrap();
        assert_eq!(configured_default_frontend(&paths), None);
    }
}
