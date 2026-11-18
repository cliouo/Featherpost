use std::path::Path;
use std::io::Read as _;

use objc2_app_kit::{NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{NSString, NSURL};

/// Opens (or, when already running, activates and re-opens) `app` without a second instance.
/// For the top-level app bundle an already running window receives `RunEvent::Reopen` and claims
/// the queued intent; a cold start goes through the launcher, which hosts the UI in place.
pub(super) fn activate_application(app: &Path) -> Result<(), &'static str> {
    validate_application(app)?;
    let app_text = app
        .to_str()
        .ok_or("frontend application path is not UTF-8")?;
    let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(app_text), true);
    let configuration = NSWorkspaceOpenConfiguration::configuration();
    configuration.setActivates(true);
    configuration.setCreatesNewApplicationInstance(false);
    let completion = block2::RcBlock::new(|_app: *mut objc2_app_kit::NSRunningApplication, error: *mut objc2_foundation::NSError| {
        if !error.is_null() {
            eprintln!("[maild/frontend] 应用激活失败，转入原生修复");
            let _ = maild_client::open_native_recovery();
        }
    });
    NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(
        &url,
        &configuration,
        Some(&completion),
    );
    Ok(())
}

/// ADR-0044: basic integrity of the installed UI bundle before the top-level app hosts the UI.
pub(super) fn validate_installed(app: &Path) -> Result<(), &'static str> {
    validate_application(app)
}

fn validate_application(app: &Path) -> Result<(), &'static str> {
    for path in [app.to_path_buf(), app.join("Contents"), app.join("Contents/MacOS")] {
        let meta = std::fs::symlink_metadata(&path).map_err(|_| "application is incomplete")?;
        if !meta.is_dir() || meta.file_type().is_symlink() { return Err("application directory is invalid"); }
        super::validate_owned_entry(&meta).map_err(|_| "unsafe application directory")?;
    }
    let plist = app.join("Contents/Info.plist");
    let meta = std::fs::symlink_metadata(&plist).map_err(|_| "application plist missing")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 65536 { return Err("application plist invalid"); }
    super::validate_owned_entry(&meta).map_err(|_| "unsafe application plist")?;
    let mut bytes = Vec::new();
    std::fs::File::open(&plist).map_err(|_| "application plist unavailable")?.take(65537)
        .read_to_end(&mut bytes).map_err(|_| "application plist unreadable")?;
    if bytes.len() > 65536 { return Err("application plist too large"); }
    let info = plist::Value::from_reader(std::io::Cursor::new(bytes)).map_err(|_| "application plist invalid")?;
    let executable = info.as_dictionary().and_then(|d| d.get("CFBundleExecutable"))
        .and_then(plist::Value::as_string).ok_or("application executable unspecified")?;
    let parts: Vec<_> = Path::new(executable).components().collect();
    if parts.len() != 1 || !matches!(parts[0], std::path::Component::Normal(_)) { return Err("application executable path invalid"); }
    let meta = std::fs::symlink_metadata(app.join("Contents/MacOS").join(executable)).map_err(|_| "application executable missing")?;
    use std::os::unix::fs::MetadataExt as _;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.mode() & 0o100 == 0 { return Err("application executable invalid"); }
    super::validate_owned_entry(&meta).map_err(|_| "unsafe application executable")
}
