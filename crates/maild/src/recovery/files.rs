use std::fs;
#[cfg(not(windows))]
use std::fs::File;
use std::io;
use std::path::Path;

pub(super) fn exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("恢复条目无法检查".into()),
    }
}

fn owned(metadata: &fs::Metadata) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err("恢复条目所有者或权限不安全".into());
        }
    }
    let _ = metadata;
    Ok(())
}

pub(super) fn check_directory(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "恢复目录不可读取")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("恢复目录必须为非符号链接目录".into());
    }
    owned(&meta)
}

pub(super) fn check_file(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "恢复文件不可读取")?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("恢复文件必须为普通文件".into());
    }
    owned(&meta)
}

pub(super) fn check_chain(root: &Path, path: &Path) -> Result<(), String> {
    if check_optional_chain(root, path)? {
        Ok(())
    } else {
        Err("应用目录不完整".into())
    }
}

pub(super) fn check_optional_chain(root: &Path, path: &Path) -> Result<bool, String> {
    check_directory(root)?;
    let relative = path.strip_prefix(root).map_err(|_| "恢复源不在应用内")?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err("恢复目录层级无效".into());
        }
        current.push(component);
        if !exists(&current)? {
            return Ok(false);
        }
        check_directory(&current)?;
    }
    Ok(true)
}

/// Create only the requested child, never silently traverse/create an unchecked ancestor.
pub(super) fn ensure_directory(path: &Path) -> Result<(), String> {
    if !exists(path)? {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|_| "恢复目录创建失败")?;
        }
        #[cfg(not(unix))]
        fs::create_dir(path).map_err(|_| "恢复目录创建失败")?;
    }
    check_directory(path)
}

pub(super) fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| "恢复目录落盘失败")?;
    let _ = path;
    Ok(())
}

/// Only called on our private, bounded staging copy, never on the user's damaged package.
pub(super) fn sync_tree(path: &Path) -> Result<(), String> {
    check_directory(path)?;
    for entry in fs::read_dir(path).map_err(|_| "暂存目录不可读取")? {
        let entry = entry.map_err(|_| "暂存条目不可读取")?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|_| "暂存条目类型不可读取")?;
        if kind.is_dir() {
            sync_tree(&path)?;
        } else {
            check_file(&path)?;
            #[cfg(not(windows))]
            let file = File::open(&path);
            // Windows FlushFileBuffers requires GENERIC_WRITE; never truncate the copy.
            #[cfg(windows)]
            let file = fs::OpenOptions::new().write(true).open(&path);
            file.and_then(|f| f.sync_all())
                .map_err(|_| "暂存文件落盘失败")?;
        }
    }
    sync_directory(path)
}

pub(super) fn mark_once(root: &Path, marker: &str) -> Result<(), String> {
    use std::io::Write as _;
    let target = root.join(marker);
    if exists(&target)? {
        return Ok(());
    }
    let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|_| "预装标记暂存失败")?;
    temp.write_all(b"1\n").map_err(|_| "预装标记写入失败")?;
    temp.as_file().sync_all().map_err(|_| "预装标记落盘失败")?;
    match temp.persist_noclobber(&target) {
        Ok(_) => {}
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("预装标记发布失败".into()),
    }
    sync_directory(root)
}

/// Directory rename with kernel-enforced no replacement (also preserves dangling symlinks).
pub(super) fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
        let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
        #[cfg(target_os = "macos")]
        let code = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let code = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        // MoveFileExW in std uses REPLACE_EXISTING, so use the no-replacement Win32 API.
        use std::os::windows::ffi::OsStrExt as _;
        let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        if from[..from.len() - 1].contains(&0) || to[..to.len() - 1].contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"));
        }
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
        };
        if ok != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (from, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exclusive rename unavailable",
        ))
    }
}
