use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

const LOCK_SUFFIX: &str = ".maild.lock";

#[derive(Debug)]
pub enum DatabaseLockError {
    InvalidDatabasePath(PathBuf),
    UnsafeDatabaseDirectory(PathBuf),
    UnsafeDatabaseFile(PathBuf),
    UnsafeLockFile(PathBuf),
    InUse { database: PathBuf, lock: PathBuf },
    Io(std::io::Error),
}

impl fmt::Display for DatabaseLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDatabasePath(path) => {
                write!(
                    f,
                    "数据库路径必须包含有效文件名，且不能是符号链接或多硬链接: {path:?}"
                )
            }
            Self::UnsafeDatabaseDirectory(path) => write!(
                f,
                "Unix 规范化数据库目录必须由当前用户拥有，且不可被组或其他用户写入: {path:?}"
            ),
            Self::UnsafeDatabaseFile(path) => write!(
                f,
                "Unix 数据库文件必须是当前用户拥有、单链接且权限为 0600 的普通文件: {path:?}"
            ),
            Self::UnsafeLockFile(path) => write!(
                f,
                "数据库锁文件必须是当前用户拥有的普通文件，权限不得开放给组或其他用户: {path:?}"
            ),
            Self::InUse { database, lock } => write!(
                f,
                "数据库正在被另一个守护/维护进程使用: {database:?}（锁 {lock:?}）"
            ),
            Self::Io(error) => write!(f, "数据库锁文件系统错误: {error}"),
        }
    }
}

impl Error for DatabaseLockError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidDatabasePath(_)
            | Self::UnsafeDatabaseDirectory(_)
            | Self::UnsafeDatabaseFile(_)
            | Self::UnsafeLockFile(_)
            | Self::InUse { .. } => None,
        }
    }
}

impl From<std::io::Error> for DatabaseLockError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// An exclusive process lock for one normalized on-disk database path.
///
/// The lock file is deliberately persistent. Removing it on drop would let a process that already
/// opened the old inode and a process that recreated the path hold different locks concurrently.
pub struct DatabaseLock {
    file: File,
    database: PathBuf,
    lock_path: PathBuf,
}

impl DatabaseLock {
    pub fn acquire(path: &Path) -> Result<Self, DatabaseLockError> {
        let database = normalize_database_path(path)?;
        let parent = database
            .parent()
            .ok_or_else(|| DatabaseLockError::InvalidDatabasePath(path.to_path_buf()))?;
        #[cfg(unix)]
        validate_unix_owned_directory(parent)?;

        let file_name = database
            .file_name()
            .ok_or_else(|| DatabaseLockError::InvalidDatabasePath(path.to_path_buf()))?;
        let mut lock_name = OsString::from(".");
        lock_name.push(file_name);
        lock_name.push(LOCK_SUFFIX);
        let lock_path = parent.join(lock_name);
        let file = open_lock_file(&lock_path)?;
        validate_lock_file(&file, &lock_path)?;

        match file.try_lock() {
            Ok(()) => Ok(Self {
                file,
                database,
                lock_path,
            }),
            Err(std::fs::TryLockError::WouldBlock) => Err(DatabaseLockError::InUse {
                database,
                lock: lock_path,
            }),
            Err(std::fs::TryLockError::Error(error)) => Err(DatabaseLockError::Io(error)),
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.database
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }
}

impl Drop for DatabaseLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(crate) fn normalize_database_path(path: &Path) -> Result<PathBuf, DatabaseLockError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(DatabaseLockError::InvalidDatabasePath(path.to_path_buf()));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                if metadata.nlink() != 1 {
                    return Err(DatabaseLockError::InvalidDatabasePath(path.to_path_buf()));
                }
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt as _;
                use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

                if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    || !windows_has_single_link(path)?
                {
                    return Err(DatabaseLockError::InvalidDatabasePath(path.to_path_buf()));
                }
            }
            return fs::canonicalize(path).map_err(DatabaseLockError::Io);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(DatabaseLockError::Io(error)),
    }

    let file_name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| DatabaseLockError::InvalidDatabasePath(path.to_path_buf()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_parent = fs::canonicalize(parent)?;
    if !fs::metadata(&canonical_parent)?.is_dir() {
        return Err(DatabaseLockError::InvalidDatabasePath(path.to_path_buf()));
    }
    Ok(canonical_parent.join(file_name))
}

#[cfg(windows)]
fn windows_has_single_link(path: &Path) -> Result<bool, DatabaseLockError> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let file = File::open(path)?;
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    let result = unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    };
    if result == 0 {
        return Err(DatabaseLockError::Io(std::io::Error::last_os_error()));
    }
    Ok(unsafe { information.assume_init() }.nNumberOfLinks == 1)
}

#[cfg(unix)]
pub(crate) fn secure_unix_database_file(path: &Path) -> Result<(), DatabaseLockError> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(DatabaseLockError::Io)?,
        Err(error) => return Err(DatabaseLockError::Io(error)),
    };
    let metadata = file.metadata()?;
    let euid = unsafe { libc::geteuid() } as u32;
    if !metadata.is_file() || metadata.uid() != euid || metadata.nlink() != 1 {
        return Err(DatabaseLockError::UnsafeDatabaseFile(path.to_path_buf()));
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = OsString::from(path.as_os_str());
        sidecar.push(suffix);
        secure_existing_unix_sidecar(Path::new(&sidecar))?;
    }
    Ok(())
}

#[cfg(unix)]
fn secure_existing_unix_sidecar(path: &Path) -> Result<(), DatabaseLockError> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DatabaseLockError::Io(error)),
    };
    let metadata = file.metadata()?;
    let euid = unsafe { libc::geteuid() } as u32;
    if !metadata.is_file() || metadata.uid() != euid || metadata.nlink() != 1 {
        return Err(DatabaseLockError::UnsafeDatabaseFile(path.to_path_buf()));
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn open_lock_file(path: &Path) -> Result<File, DatabaseLockError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        // Keeping FILE_SHARE_DELETE disabled is part of the lock identity guarantee: while this
        // handle is alive, another process must not replace the path with a separately lockable
        // inode/file object.
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path).map_err(|error| {
        if error.raw_os_error() == Some(libc_eloop()) {
            DatabaseLockError::UnsafeLockFile(path.to_path_buf())
        } else {
            DatabaseLockError::Io(error)
        }
    })
}

#[cfg(unix)]
const fn libc_eloop() -> i32 {
    libc::ELOOP
}

#[cfg(not(unix))]
const fn libc_eloop() -> i32 {
    i32::MIN
}

fn validate_lock_file(file: &File, path: &Path) -> Result<(), DatabaseLockError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(DatabaseLockError::UnsafeLockFile(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let euid = unsafe { libc::geteuid() } as u32;
        if metadata.uid() != euid || metadata.permissions().mode() & 0o077 != 0 {
            return Err(DatabaseLockError::UnsafeLockFile(path.to_path_buf()));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(DatabaseLockError::UnsafeLockFile(path.to_path_buf()));
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn validate_unix_owned_directory(path: &Path) -> Result<(), DatabaseLockError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let canonical = fs::canonicalize(path)?;
    let euid = unsafe { libc::geteuid() } as u32;
    let leaf = fs::symlink_metadata(&canonical)?;
    if !leaf.file_type().is_dir() || leaf.uid() != euid || leaf.permissions().mode() & 0o022 != 0 {
        return Err(DatabaseLockError::UnsafeDatabaseDirectory(
            path.to_path_buf(),
        ));
    }

    let mut ancestor = canonical.as_path();
    loop {
        let metadata = fs::symlink_metadata(ancestor)?;
        let mode = metadata.permissions().mode();
        let trusted_owner = metadata.uid() == euid || metadata.uid() == 0;
        let public_writable = mode & 0o022 != 0;
        let sticky_root = mode & 0o1000 != 0 && metadata.uid() == 0;
        if !metadata.file_type().is_dir() || !trusted_owner || (public_writable && !sticky_root) {
            return Err(DatabaseLockError::UnsafeDatabaseDirectory(
                path.to_path_buf(),
            ));
        }
        if ancestor == Path::new("/") {
            break;
        }
        ancestor = ancestor
            .parent()
            .ok_or_else(|| DatabaseLockError::UnsafeDatabaseDirectory(path.to_path_buf()))?;
    }
    Ok(())
}
