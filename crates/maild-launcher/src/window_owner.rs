//! Single native recovery window per platform runtime, shared across Menu and Agent callers.
#[cfg(target_os = "macos")]
pub(crate) struct WindowOwner {
    _file: std::fs::File,
}

#[cfg(target_os = "macos")]
pub(crate) fn acquire() -> Result<Option<WindowOwner>, &'static str> {
    let paths = maild_paths::AppPaths::discover().map_err(|_| "无法确定修复窗口目录")?;
    acquire_at(&paths, super::platform::focus)
}

#[cfg(target_os = "macos")]
fn acquire_at(
    paths: &maild_paths::AppPaths,
    focus: impl FnOnce(i32),
) -> Result<Option<WindowOwner>, &'static str> {
    use maild::agent_lifecycle::{AgentLifecycleLock, AgentLifecycleStatus};
    use std::io::{Read as _, Seek as _, Write as _};
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let root = if paths.runtime_dir.starts_with(&paths.data_dir) {
        &paths.data_dir
    } else {
        &paths.runtime_dir
    };
    match AgentLifecycleLock::probe(root, &paths.runtime_dir)
        .map_err(|_| "修复窗口目录不可安全使用")?
    {
        AgentLifecycleStatus::Idle if !paths.runtime_dir.exists() => {
            let _prepare = AgentLifecycleLock::acquire(root, &paths.runtime_dir)
                .map_err(|_| "后台正在启动，请稍后打开修复窗口")?;
        }
        _ => {}
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(paths.runtime_dir.join(".recovery-ui.lock"))
        .map_err(|_| "修复窗口锁不可用")?;
    let meta = file.metadata().map_err(|_| "修复窗口锁不可检查")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err("修复窗口锁类型或权限无效");
    }
    match file.try_lock() {
        Ok(()) => {
            file.set_len(0).map_err(|_| "修复窗口状态不可写")?;
            write!(file, "{}", std::process::id()).map_err(|_| "修复窗口状态不可写")?;
            file.flush().map_err(|_| "修复窗口状态不可写")?;
            Ok(Some(WindowOwner { _file: file }))
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut pid = String::new();
            file.rewind().map_err(|_| "修复窗口状态不可读")?;
            file.take(16)
                .read_to_string(&mut pid)
                .map_err(|_| "修复窗口状态不可读")?;
            if let Ok(pid) = pid.parse::<i32>() {
                focus(pid);
            }
            Ok(None)
        }
        Err(_) => Err("修复窗口锁不可用"),
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) struct WindowOwner;
#[cfg(not(target_os = "macos"))]
pub(crate) fn acquire() -> Result<Option<WindowOwner>, &'static str> {
    Ok(Some(WindowOwner))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn second_window_focuses_owner_and_release_allows_reopen() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let paths = maild_paths::AppPaths::from_roots(
            &root.join("config"),
            &root.join("data"),
            None,
            None,
            &root,
        );
        let owner = acquire_at(&paths, |_| {
            panic!("first window cannot focus another owner")
        })
        .unwrap()
        .unwrap();
        let focused = std::cell::Cell::new(None);
        assert!(acquire_at(&paths, |pid| focused.set(Some(pid)))
            .unwrap()
            .is_none());
        assert_eq!(focused.get(), Some(std::process::id() as i32));
        drop(owner);
        assert!(
            acquire_at(&paths, |_| panic!("stale PID cannot block reopen"))
                .unwrap()
                .is_some()
        );
        let lock = paths.runtime_dir.join(".recovery-ui.lock");
        std::fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), &lock).unwrap();
        assert!(acquire_at(&paths, |_| panic!("unsafe lock cannot focus")).is_err());
        assert!(!root.join("outside").exists());
    }
}
