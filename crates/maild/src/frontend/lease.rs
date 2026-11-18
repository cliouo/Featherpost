//! Cross-process ownership of the managed frontend directory, independent of any database.
use std::fs::{File, OpenOptions};
use std::path::Path;

use cordis::Service;

pub(crate) struct FrontendDirectoryLease {
    _file: File,
}

impl Service for FrontendDirectoryLease {
    fn name(&self) -> &'static str {
        "frontend.directory-lease"
    }
}

impl FrontendDirectoryLease {
    pub(crate) fn acquire(root: &Path) -> Result<Self, String> {
        let meta = std::fs::symlink_metadata(root).map_err(|_| "前端目录不可读取")?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("前端目录类型无效".into());
        }
        super::validate_owned_entry(&meta)?;
        let path = root.join(".owner.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let file = options.open(path).map_err(|_| "前端目录占用锁不可打开")?;
        let meta = file.metadata().map_err(|_| "前端目录占用锁不可检查")?;
        if !meta.is_file() {
            return Err("前端目录占用锁类型无效".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            if meta.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                return Err("前端目录占用锁不可为重解析点".into());
            }
        }
        crate::plugin_state::check_owned_file(&meta)?;
        file.try_lock()
            .map_err(|_| "前端目录仍被后台或恢复任务占用")?;
        // Never unlink this inode. An online recovery holds an Arc through daemon shutdown.
        Ok(Self { _file: file })
    }
}
