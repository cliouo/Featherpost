//! Standalone offline direct-launch handoff and daemon-independent native recovery (Angle 2A).
//!
//! When a user launches a standalone managed frontend bundle (`Featherpost Studio.app` or
//! `Featherpost UI.app`) directly while the local daemon socket is offline, this module hands off
//! cleanly to the installed top-level `Featherpost.app` so the native launcher can start the
//! background Agent and host the single-Dock window. It also powers the daemon-independent
//! "Open repair window" command in Settings so offline windows always have a working recovery exit.

#[cfg(any(target_os = "macos", test))]
use std::ffi::OsStr;
#[cfg(any(target_os = "macos", test))]
use std::io::Read as _;
#[cfg(any(target_os = "macos", test))]
use std::path::{Component, Path, PathBuf};
#[cfg(any(target_os = "macos", all(unix, test)))]
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};

/// Minimum interval between standalone-to-host handoffs, preventing LaunchServices ping-pong loops.
#[cfg(any(target_os = "macos", all(unix, test)))]
const HANDOFF_COOLDOWN: Duration = Duration::from_secs(10);

#[cfg(target_os = "macos")]
fn handoff_guard_env() -> String {
    format!("{}_HANDOFF_GUARD", maild_brand::ENV_PREFIX)
}

#[cfg(any(target_os = "macos", all(unix, test)))]
fn handoff_guard_filename() -> String {
    format!(".{}-standalone-handoff", maild_brand::SLUG)
}

/// Returns the enclosing `*.app` bundle path when `exe` is `<dir>/<Name>.app/Contents/MacOS/<bin>`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn enclosing_app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()? != OsStr::new("MacOS") {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != OsStr::new("Contents") {
        return None;
    }
    let app = contents.parent()?;
    let name = app.file_name()?.to_str()?;
    (name.ends_with(".app") && name.len() > ".app".len()).then(|| app.to_path_buf())
}

/// `true` only when `exe` lives inside a standalone `.app` bundle (not a bare dev binary such as
/// `target/debug/...`, and not the top-level `APP_BUNDLE_NAME`).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn is_standalone_app_bundle(exe: &Path) -> bool {
    enclosing_app_bundle(exe)
        .and_then(|app| app.file_name().map(|name| name != OsStr::new(maild_brand::APP_BUNDLE_NAME)))
        .unwrap_or(false)
}

/// Handoff is allowed only when the configured default frontend matches this frontend or cannot be
/// determined (e.g. fresh install or corrupt `.default.json` that needs native recovery).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn should_handoff_for_default(
    configured_default: Option<&str>,
    self_frontend_id: &str,
) -> bool {
    configured_default.is_none_or(|default| default == self_frontend_id)
}

/// Validate a candidate top-level `APP_BUNDLE_NAME` path discovered via LaunchServices before
/// handing off or launching `--repair`. Returns the validated launcher executable path.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn validate_host_app_bundle(
    app: &Path,
    current_exe: Option<&Path>,
) -> Result<PathBuf, &'static str> {
    if !app.is_absolute() {
        return Err("主应用路径不是绝对路径");
    }
    if app.file_name() != Some(OsStr::new(maild_brand::APP_BUNDLE_NAME)) {
        return Err("主应用包名称不匹配");
    }
    if app
        .components()
        .any(|component| component == Component::Normal(OsStr::new(".Trash")))
    {
        return Err("拒绝从废纸篓启动主应用");
    }
    if let Some(self_app) = current_exe.and_then(enclosing_app_bundle) {
        if let (Ok(candidate), Ok(own)) = (std::fs::canonicalize(app), std::fs::canonicalize(&self_app)) {
            if candidate == own {
                return Err("拒绝自递归拉起同一应用包");
            }
        }
    }

    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let launcher = macos.join(maild_brand::LAUNCHER_BIN);
    let plist_path = contents.join("Info.plist");

    for dir in [app, contents.as_path(), macos.as_path()] {
        let meta = std::fs::symlink_metadata(dir).map_err(|_| "主应用目录不完整")?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err("主应用目录类型无效");
        }
        check_host_entry_permissions(&meta, false)?;
    }

    let launcher_meta =
        std::fs::symlink_metadata(&launcher).map_err(|_| "主应用启动器缺失")?;
    if launcher_meta.file_type().is_symlink() || !launcher_meta.is_file() {
        return Err("主应用启动器类型无效");
    }
    check_host_entry_permissions(&launcher_meta, true)?;

    let plist_meta =
        std::fs::symlink_metadata(&plist_path).map_err(|_| "主应用 Info.plist 缺失")?;
    if plist_meta.file_type().is_symlink()
        || !plist_meta.is_file()
        || plist_meta.len() == 0
        || plist_meta.len() > 65_536
    {
        return Err("主应用 Info.plist 无效");
    }
    check_host_entry_permissions(&plist_meta, false)?;

    let mut bytes = Vec::new();
    std::fs::File::open(&plist_path)
        .map_err(|_| "主应用 Info.plist 不可读取")?
        .take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|_| "主应用 Info.plist 读取失败")?;
    if bytes.len() > 65_536 {
        return Err("主应用 Info.plist 过大");
    }
    let parsed = plist::Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|_| "主应用 Info.plist 解析失败")?;
    let dict = parsed
        .as_dictionary()
        .ok_or("主应用 Info.plist 格式无效")?;
    for (key, expected) in [
        ("CFBundleIdentifier", maild_brand::APP_BUNDLE_ID),
        ("CFBundleExecutable", maild_brand::LAUNCHER_BIN),
        ("CFBundlePackageType", "APPL"),
    ] {
        if dict.get(key).and_then(plist::Value::as_string) != Some(expected) {
            return Err("主应用 Info.plist 身份不匹配");
        }
    }

    Ok(launcher)
}

#[cfg(any(target_os = "macos", all(unix, test)))]
fn check_host_entry_permissions(
    meta: &std::fs::Metadata,
    require_executable: bool,
) -> Result<(), &'static str> {
    use std::os::unix::fs::MetadataExt as _;
    let euid = unsafe { libc::geteuid() };
    let uid = meta.uid();
    if uid != euid && uid != 0 {
        return Err("主应用所有者无效");
    }
    let mode = meta.mode();
    if mode & 0o022 != 0 {
        return Err("主应用权限过宽");
    }
    if require_executable {
        let executable = if uid == euid {
            mode & 0o100 != 0
        } else {
            mode & 0o001 != 0
        };
        if !executable {
            return Err("主应用启动器缺少执行权限");
        }
    }
    Ok(())
}

#[cfg(all(not(unix), test))]
fn check_host_entry_permissions(
    _meta: &std::fs::Metadata,
    _require_executable: bool,
) -> Result<(), &'static str> {
    Ok(())
}

#[cfg(any(target_os = "macos", all(unix, test)))]
fn is_safe_private_dir(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    !meta.file_type().is_symlink()
        && meta.is_dir()
        && meta.uid() == unsafe { libc::geteuid() }
        && meta.mode() & 0o022 == 0
}

/// Cross-process re-entry/loop guard backed by a private `0600` `O_NOFOLLOW` timestamp file.
/// Returns `Ok(true)` when handoff may proceed (recording `now_ms`), `Ok(false)` when a handoff
/// already ran within `cooldown` or another launch holds the guard lock.
#[cfg(any(target_os = "macos", all(unix, test)))]
pub(crate) fn check_and_arm_cooldown(
    guard_dir: &Path,
    now_ms: u128,
    cooldown: Duration,
) -> Result<bool, &'static str> {
    use std::io::{Seek as _, Write as _};
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    if !is_safe_private_dir(guard_dir) {
        return Err("防重入守卫目录不安全");
    }
    let path = guard_dir.join(handoff_guard_filename());
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| "防重入守卫文件不可打开")?;
    let meta = file.metadata().map_err(|_| "防重入守卫文件不可检查")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err("防重入守卫文件权限无效");
    }
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
        Err(_) => return Err("防重入守卫锁不可用"),
    }
    let mut raw = String::new();
    let _ = file.rewind();
    let _ = std::io::Read::by_ref(&mut file)
        .take(64)
        .read_to_string(&mut raw);
    if let Ok(last_ms) = raw.trim().parse::<u128>() {
        if now_ms >= last_ms && now_ms - last_ms < cooldown.as_millis() {
            return Ok(false);
        }
    }
    file.set_len(0).map_err(|_| "防重入守卫不可写")?;
    file.rewind().map_err(|_| "防重入守卫不可写")?;
    write!(file, "{now_ms}").map_err(|_| "防重入守卫不可写")?;
    file.flush().map_err(|_| "防重入守卫不可写")?;
    Ok(true)
}

#[cfg(target_os = "macos")]
fn is_unix_socket_online(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

#[cfg(target_os = "macos")]
fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

#[cfg(target_os = "macos")]
fn pick_guard_dir(paths: &maild_paths::AppPaths) -> Option<PathBuf> {
    if is_safe_private_dir(&paths.runtime_dir) {
        return Some(paths.runtime_dir.clone());
    }
    let temp = std::env::temp_dir();
    is_safe_private_dir(&temp).then_some(temp)
}

#[cfg(any(target_os = "macos", all(unix, test)))]
struct HandoffContext<'a> {
    custom_connection: bool,
    has_env_override: bool,
    system_home: bool,
    in_place_layout: bool,
    current_exe: Option<&'a Path>,
    socket_online: bool,
    configured_default: Option<&'a str>,
    self_frontend_id: &'a str,
    env_guard_set: bool,
    guard_dir: Option<&'a Path>,
    now_ms: u128,
}

#[cfg(any(target_os = "macos", all(unix, test)))]
fn run_handoff_decision(
    ctx: &HandoffContext<'_>,
    lookup_host: impl FnOnce() -> Option<PathBuf>,
    open_host: impl FnOnce(&Path) -> Result<(), &'static str>,
) -> bool {
    if ctx.custom_connection
        || ctx.has_env_override
        || !ctx.system_home
        || ctx.in_place_layout
        || ctx.socket_online
        || ctx.env_guard_set
    {
        return false;
    }
    let Some(exe) = ctx.current_exe else {
        return false;
    };
    if !is_standalone_app_bundle(exe) {
        return false;
    }
    if !should_handoff_for_default(ctx.configured_default, ctx.self_frontend_id) {
        return false;
    }
    let Some(host_app) = lookup_host() else {
        return false;
    };
    if let Err(reason) = validate_host_app_bundle(&host_app, Some(exe)) {
        eprintln!("[maild-ui] 跳过离线移交主应用: {reason}");
        return false;
    }
    let Some(guard_dir) = ctx.guard_dir else {
        return false;
    };
    match check_and_arm_cooldown(guard_dir, ctx.now_ms, HANDOFF_COOLDOWN) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("[maild-ui] 离线移交冷却期内，留在当前独立窗口");
            return false;
        }
        Err(reason) => {
            eprintln!("[maild-ui] 离线移交守卫不可用: {reason}");
            return false;
        }
    }
    match open_host(&host_app) {
        Ok(()) => true,
        Err(reason) => {
            eprintln!("[maild-ui] 离线移交主应用失败: {reason}");
            false
        }
    }
}

/// Defense-in-depth (Angle 2A): when launched directly as a standalone `.app` bundle with default
/// platform paths while the local daemon socket is offline, hand off to `APP_BUNDLE_NAME` via
/// LaunchServices (`activates = true`) and return `true` so this standalone process can exit
/// cleanly.
#[cfg(target_os = "macos")]
pub fn maybe_handoff_to_host_app(
    custom_connection: bool,
    endpoint: &maild_client::Endpoint,
) -> bool {
    let maild_client::Endpoint::Unix { path: socket_path } = endpoint else {
        return false;
    };
    let in_place_layout = match maild_client::NativeAppLayout::discover() {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(_) => return false,
    };
    let current_exe = std::env::current_exe().ok();
    let Ok(paths) = maild_paths::AppPaths::discover() else {
        return false;
    };
    let configured_default = maild_client::configured_default_frontend(&paths);
    let guard_dir = pick_guard_dir(&paths);
    let ctx = HandoffContext {
        custom_connection,
        has_env_override: maild_client::NativeAppLayout::has_custom_connection(),
        system_home: maild_client::system_home(),
        in_place_layout,
        current_exe: current_exe.as_deref(),
        socket_online: is_unix_socket_online(socket_path),
        configured_default: configured_default.as_deref(),
        self_frontend_id: crate::FRONTEND_ID,
        env_guard_set: std::env::var_os(handoff_guard_env()).is_some_and(|v| !v.is_empty()),
        guard_dir: guard_dir.as_deref(),
        now_ms: now_millis(),
    };
    run_handoff_decision(&ctx, macos::lookup_host_app_bundle, macos::open_host_app)
}

#[cfg(not(target_os = "macos"))]
pub fn maybe_handoff_to_host_app(
    _custom_connection: bool,
    _endpoint: &maild_client::Endpoint,
) -> bool {
    false
}

/// Daemon-independent native recovery launcher (Angle 2A settings button fallback).
///
/// Inside `APP_BUNDLE_NAME` (in-place UI or Studio), delegates directly to
/// `maild_client::open_native_recovery()`. From a standalone bundle on macOS, looks up
/// `APP_BUNDLE_ID` via `NSWorkspace`, validates it, and launches `featherpost-launcher --repair`
/// (falling back to opening `APP_BUNDLE_NAME` via `NSWorkspace` if direct spawn fails).
pub fn open_native_recovery_directly() -> Result<(), &'static str> {
    if maild_client::open_native_recovery().is_ok() {
        return Ok(());
    }
    open_host_recovery_fallback()
}

#[cfg(target_os = "macos")]
fn open_host_recovery_fallback() -> Result<(), &'static str> {
    static LAST_OPEN: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    if maild_client::NativeAppLayout::has_custom_connection() {
        return Err("自定义连接不能启动平台修复入口");
    }
    if !maild_client::system_home() {
        return Err("隔离环境不能启动平台修复入口");
    }
    let app = macos::lookup_host_app_bundle().ok_or("未找到已安装的主应用包")?;
    let launcher = validate_host_app_bundle(&app, None)?;
    let mut last = LAST_OPEN.lock().map_err(|_| "修复入口状态不可用")?;
    if last.is_some_and(|at| at.elapsed() < Duration::from_secs(1)) {
        return Ok(());
    }
    match maild_client::native_child_command(&launcher)
        .arg("--repair")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            *last = Some(std::time::Instant::now());
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(_) => {
            macos::open_host_app(&app)?;
            *last = Some(std::time::Instant::now());
            Ok(())
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn open_host_recovery_fallback() -> Result<(), &'static str> {
    Err("当前平台不支持离线修复入口")
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
    use objc2_foundation::{NSError, NSString, NSURL};

    pub(super) fn lookup_host_app_bundle() -> Option<PathBuf> {
        objc2::rc::autoreleasepool(|_| {
            let id = NSString::from_str(maild_brand::APP_BUNDLE_ID);
            let url = NSWorkspace::sharedWorkspace().URLForApplicationWithBundleIdentifier(&id)?;
            if !url.isFileURL() {
                return None;
            }
            let path = url.path()?;
            Some(PathBuf::from(path.to_string()))
        })
    }

    pub(super) fn open_host_app(app: &Path) -> Result<(), &'static str> {
        let text = app.to_str().ok_or("主应用路径不是有效 UTF-8")?;
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(text), true);
        let configuration = NSWorkspaceOpenConfiguration::configuration();
        configuration.setActivates(true);
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
        match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(true) => Ok(()),
            Ok(false) => Err("LaunchServices 拒绝打开主应用"),
            Err(_) => Err("等待主应用启动响应超时"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_host_app(root: &Path, bundle_name: &str, bundle_id: &str, exe_name: &str) -> PathBuf {
        let app = root.join(bundle_name);
        let launcher = app.join("Contents/MacOS").join(exe_name);
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::write(&launcher, b"fixture launcher").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{bundle_id}</string>
<key>CFBundleExecutable</key><string>{exe_name}</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#
            ),
        )
        .unwrap();
        app
    }

    #[test]
    fn standalone_bundle_detection_distinguishes_app_bundles_from_dev_and_host_binaries() {
        assert!(is_standalone_app_bundle(Path::new(
            "/Users/a/Library/Application Support/featherpost/frontends/dev.featherpost.studio/Featherpost Studio.app/Contents/MacOS/featherpost-studio"
        )));
        assert!(is_standalone_app_bundle(Path::new(
            "/Users/a/Library/Application Support/featherpost/frontends/dev.featherpost.ui/Featherpost UI.app/Contents/MacOS/featherpost-ui"
        )));
        assert!(!is_standalone_app_bundle(Path::new(
            "/Users/a/repo/target/debug/featherpost-ui"
        )));
        assert!(!is_standalone_app_bundle(&Path::new("/Applications")
            .join(maild_brand::APP_BUNDLE_NAME)
            .join("Contents/MacOS")
            .join(maild_client::STUDIO_BIN)));
    }

    #[test]
    fn default_frontend_filter_allows_match_or_undetermined_and_blocks_mismatch() {
        assert!(should_handoff_for_default(None, maild_client::STUDIO_FRONTEND_ID));
        assert!(should_handoff_for_default(
            Some(maild_client::STUDIO_FRONTEND_ID),
            maild_client::STUDIO_FRONTEND_ID
        ));
        assert!(!should_handoff_for_default(
            Some(maild_brand::BUNDLED_FRONTEND_ID),
            maild_client::STUDIO_FRONTEND_ID
        ));
        assert!(should_handoff_for_default(
            Some(maild_brand::BUNDLED_FRONTEND_ID),
            maild_brand::BUNDLED_FRONTEND_ID
        ));
    }

    #[test]
    fn host_app_validation_accepts_valid_bundle_and_rejects_tampered_or_trash_paths() {
        let root = tempfile::tempdir().unwrap();
        let valid = make_host_app(
            root.path(),
            maild_brand::APP_BUNDLE_NAME,
            maild_brand::APP_BUNDLE_ID,
            maild_brand::LAUNCHER_BIN,
        );
        let launcher = validate_host_app_bundle(&valid, None).unwrap();
        assert_eq!(
            launcher,
            valid.join("Contents/MacOS").join(maild_brand::LAUNCHER_BIN)
        );

        // Wrong bundle name
        let renamed = make_host_app(
            root.path(),
            "Other.app",
            maild_brand::APP_BUNDLE_ID,
            maild_brand::LAUNCHER_BIN,
        );
        assert!(validate_host_app_bundle(&renamed, None).is_err());

        // Inside .Trash
        let trash_dir = root.path().join(".Trash");
        let trashed = make_host_app(
            &trash_dir,
            maild_brand::APP_BUNDLE_NAME,
            maild_brand::APP_BUNDLE_ID,
            maild_brand::LAUNCHER_BIN,
        );
        assert!(validate_host_app_bundle(&trashed, None).is_err());

        // Wrong CFBundleIdentifier
        let wrong_id_dir = root.path().join("wrong-id");
        let wrong_id = make_host_app(
            &wrong_id_dir,
            maild_brand::APP_BUNDLE_NAME,
            "org.example.impostor",
            maild_brand::LAUNCHER_BIN,
        );
        assert!(validate_host_app_bundle(&wrong_id, None).is_err());

        // Self-launch guard
        assert!(validate_host_app_bundle(&valid, Some(&launcher)).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // Symlinked bundle rejected
            let link = root.path().join("link-parent").join(maild_brand::APP_BUNDLE_NAME);
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&valid, &link).unwrap();
            assert!(validate_host_app_bundle(&link, None).is_err());

            // Group/world writable launcher rejected
            std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert!(validate_host_app_bundle(&valid, None).is_err());
            std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(validate_host_app_bundle(&valid, None).is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn cooldown_guard_blocks_rapid_reentry_and_rearms_after_expiry() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let guard_dir = root.path().join("run");
        std::fs::create_dir(&guard_dir).unwrap();
        std::fs::set_permissions(&guard_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(check_and_arm_cooldown(&guard_dir, 1_000_000, HANDOFF_COOLDOWN).unwrap());
        assert!(!check_and_arm_cooldown(&guard_dir, 1_000_500, HANDOFF_COOLDOWN).unwrap());
        assert!(!check_and_arm_cooldown(&guard_dir, 1_009_999, HANDOFF_COOLDOWN).unwrap());
        assert!(check_and_arm_cooldown(&guard_dir, 1_010_000, HANDOFF_COOLDOWN).unwrap());

        // Symlinked guard file is rejected without following.
        let guard_file = guard_dir.join(handoff_guard_filename());
        std::fs::remove_file(&guard_file).unwrap();
        let outside = root.path().join("outside");
        std::os::unix::fs::symlink(&outside, &guard_file).unwrap();
        assert!(check_and_arm_cooldown(&guard_dir, 1_020_000, HANDOFF_COOLDOWN).is_err());
        assert!(!outside.exists());
    }

    #[cfg(unix)]
    #[test]
    fn handoff_decision_triggers_only_when_standalone_offline_default_matches_and_unguarded() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let host = make_host_app(
            root.path(),
            maild_brand::APP_BUNDLE_NAME,
            maild_brand::APP_BUNDLE_ID,
            maild_brand::LAUNCHER_BIN,
        );
        let guard_dir = root.path().join("run");
        std::fs::create_dir(&guard_dir).unwrap();
        std::fs::set_permissions(&guard_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let standalone_exe = root
            .path()
            .join("Featherpost Studio.app/Contents/MacOS/featherpost-studio");
        let base_ctx = || HandoffContext {
            custom_connection: false,
            has_env_override: false,
            system_home: true,
            in_place_layout: false,
            current_exe: Some(&standalone_exe),
            socket_online: false,
            configured_default: Some(maild_client::STUDIO_FRONTEND_ID),
            self_frontend_id: maild_client::STUDIO_FRONTEND_ID,
            env_guard_set: false,
            guard_dir: Some(&guard_dir),
            now_ms: 5_000_000,
        };

        // Online socket -> no handoff.
        let mut ctx = base_ctx();
        ctx.socket_online = true;
        assert!(!run_handoff_decision(&ctx, || panic!("should not lookup"), |_| Ok(())));

        // In-place layout -> no handoff.
        let mut ctx = base_ctx();
        ctx.in_place_layout = true;
        assert!(!run_handoff_decision(&ctx, || panic!("should not lookup"), |_| Ok(())));

        // Custom connection -> no handoff.
        let mut ctx = base_ctx();
        ctx.custom_connection = true;
        assert!(!run_handoff_decision(&ctx, || panic!("should not lookup"), |_| Ok(())));

        // Different default frontend -> no handoff.
        let mut ctx = base_ctx();
        ctx.configured_default = Some(maild_brand::BUNDLED_FRONTEND_ID);
        assert!(!run_handoff_decision(&ctx, || panic!("should not lookup"), |_| Ok(())));

        // Env guard set -> no handoff.
        let mut ctx = base_ctx();
        ctx.env_guard_set = true;
        assert!(!run_handoff_decision(&ctx, || panic!("should not lookup"), |_| Ok(())));

        // Valid standalone offline launch -> opens host app and arms cooldown.
        let opened = std::cell::RefCell::new(Vec::new());
        let ctx = base_ctx();
        assert!(run_handoff_decision(
            &ctx,
            || Some(host.clone()),
            |path| {
                opened.borrow_mut().push(path.to_path_buf());
                Ok(())
            }
        ));
        assert_eq!(opened.borrow().as_slice(), std::slice::from_ref(&host));

        // Immediate re-entry within cooldown -> blocked by guard.
        let mut retry = base_ctx();
        retry.now_ms = 5_001_000;
        assert!(!run_handoff_decision(
            &retry,
            || Some(host.clone()),
            |_| panic!("cooldown must block re-entry")
        ));

        // After cooldown, undetermined default also hands off; open error falls back cleanly.
        let mut after = base_ctx();
        after.configured_default = None;
        after.now_ms = 5_020_000;
        assert!(!run_handoff_decision(
            &after,
            || Some(host.clone()),
            |_| Err("workspace failed")
        ));
    }
}
