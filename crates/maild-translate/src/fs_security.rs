//! Cross-platform file security primitives:
//! - No-follow handle opening (`O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK` on Unix;
//!   `FILE_FLAG_OPEN_REPARSE_POINT` on Windows)
//! - Handle-level metadata & ownership verification (zero TOCTOU): regular file, current-user
//!   owner, strict `0600` mode on Unix / explicit user-private protected DACL on Windows,
//!   single hard link (`nlink == 1`)
//! - Bounded read (`max_bytes + 1`)
//! - Cross-process exclusive file lock (`File::lock` / `File::unlock`)
//! - Atomic publish via exclusive private temp file + `sync_all` + `rename` + parent dir sync

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use crate::error::TranslateError;

/// Guard holding a cross-process exclusive file lock on a private lock file.
pub struct ProcessFileLock {
    file: File,
}

impl Drop for ProcessFileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Generate a cryptographically random lowercase hex token using `getrandom`.
pub fn random_hex_token(byte_len: usize) -> Result<String, TranslateError> {
    let mut buf = vec![0u8; byte_len];
    getrandom::fill(&mut buf)
        .map_err(|_| TranslateError::Io("system CSPRNG unavailable".into()))?;
    Ok(hex::encode(buf))
}

/// Ensure a private directory exists with owner-only permissions (`0700` on Unix,
/// current-user-only protected DACL on Windows).
pub fn ensure_private_dir(dir: &Path) -> Result<(), TranslateError> {
    if !dir.exists() {
        fs::create_dir_all(dir)
            .map_err(|_| TranslateError::Io("failed to create private directory".into()))?;
    }
    let meta = fs::symlink_metadata(dir)
        .map_err(|_| TranslateError::Io("failed to inspect directory".into()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(TranslateError::ConfigUnavailable(
            "directory path is a symlink or not a directory".into(),
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return Err(TranslateError::ConfigUnavailable(
                "directory is not owned by current user".into(),
            ));
        }
        if meta.mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .map_err(|_| TranslateError::Io("failed to tighten directory permissions".into()))?;
        }
    }

    #[cfg(windows)]
    {
        win_acl::set_user_private_acl_on_path(dir)?;
    }

    Ok(())
}

/// Acquire a cross-process exclusive lock on a private lock file inside `dir`.
pub fn acquire_process_lock(dir: &Path, lock_file_name: &str) -> Result<ProcessFileLock, TranslateError> {
    ensure_private_dir(dir)?;
    let lock_path = dir.join(lock_file_name);

    let file = open_private_lock_file(&lock_path)?;
    verify_open_handle_security(&file, false)?;
    file.lock()
        .map_err(|_| TranslateError::Io("failed to acquire cross-process config lock".into()))?;
    Ok(ProcessFileLock { file })
}

fn open_private_lock_file(path: &Path) -> Result<File, TranslateError> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        opts.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = opts.open(path).map_err(|err| {
        if is_symlink_open_error(&err) {
            TranslateError::ConfigUnavailable("lock file is a symlink".into())
        } else {
            TranslateError::Io("failed to open lock file".into())
        }
    })?;
    #[cfg(windows)]
    {
        win_acl::set_user_private_acl_on_handle(&file)?;
    }
    Ok(file)
}

/// Open an existing config file with no-follow semantics, verify handle-level security
/// (regular file, current user owner, `0600` mode / Windows user-private DACL, `nlink == 1`),
/// and read at most `max_bytes + 1` bytes.
///
/// Returns `Ok(None)` if the file does not exist (`NotFound`).
pub fn read_private_file_bounded(
    path: &Path,
    max_bytes: u64,
) -> Result<Option<Vec<u8>>, TranslateError> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    let file = match opts.open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) if is_symlink_open_error(&err) => {
            return Err(TranslateError::ConfigUnavailable(
                "config path is a symlink".into(),
            ));
        }
        Err(_) => {
            return Err(TranslateError::ConfigUnavailable(
                "failed to open config file".into(),
            ));
        }
    };

    verify_open_handle_security(&file, true)?;

    let mut buf = Vec::new();
    let cap = max_bytes.saturating_add(1);
    file.take(cap)
        .read_to_end(&mut buf)
        .map_err(|_| TranslateError::ConfigUnavailable("failed to read config file".into()))?;
    if buf.len() as u64 > max_bytes {
        return Err(TranslateError::ConfigUnavailable(
            "config file exceeds 64 KiB limit".into(),
        ));
    }

    Ok(Some(buf))
}

/// Verify security invariants directly on an opened [`File`] handle (no path TOCTOU).
///
/// - Unix: regular file, owned by current effective UID, `nlink == 1`, and if `strict_mode` is
///   true exact `0600` mode (otherwise no group/other bits).
/// - Windows: regular non-reparse file, `nNumberOfLinks == 1`, owned by current token user SID,
///   and protected DACL granting access only to current user SID.
pub fn verify_open_handle_security(file: &File, strict_mode: bool) -> Result<(), TranslateError> {
    let metadata = file
        .metadata()
        .map_err(|_| TranslateError::ConfigUnavailable("failed to inspect file handle".into()))?;
    if !metadata.is_file() {
        return Err(TranslateError::ConfigUnavailable(
            "path is not a regular file".into(),
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let euid = unsafe { libc::geteuid() };
        if metadata.uid() != euid {
            return Err(TranslateError::ConfigUnavailable(
                "file is not owned by current user".into(),
            ));
        }
        let perm = metadata.mode() & 0o777;
        if strict_mode {
            if perm != 0o600 {
                return Err(TranslateError::ConfigUnavailable(format!(
                    "file mode must be 0600 (found {:04o})",
                    perm
                )));
            }
        } else if perm & 0o077 != 0 {
            return Err(TranslateError::ConfigUnavailable(format!(
                "file mode too permissive ({:04o})",
                perm
            )));
        }
        if metadata.nlink() != 1 {
            return Err(TranslateError::ConfigUnavailable(format!(
                "file must have exactly 1 hard link (found {})",
                metadata.nlink()
            )));
        }
    }

    #[cfg(windows)]
    {
        let _ = strict_mode;
        win_acl::verify_windows_file_handle(file)?;
    }

    Ok(())
}

/// Atomically publish `body` to `target` inside a private directory using:
/// 1. Exclusive (`create_new`) `0600` / user-private-ACL temp file in `target`'s parent directory
/// 2. Write + `sync_all()`
/// 3. Atomic `rename` (`MoveFileExW` on Windows)
/// 4. Parent directory `sync_all()` (on Unix)
pub fn publish_private_atomically(target: &Path, body: &[u8]) -> Result<(), TranslateError> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| TranslateError::Io("target path missing parent directory".into()))?;
    ensure_private_dir(parent)?;

    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config");
    let token = random_hex_token(12)?;
    let temp: PathBuf = parent.join(format!(".{file_name}.tmp-{token}"));

    let write_res = (|| -> Result<(), TranslateError> {
        let mut temp_file = create_exclusive_private_file(&temp)?;
        temp_file
            .write_all(body)
            .map_err(|_| TranslateError::Io("failed to write temporary file".into()))?;
        temp_file
            .sync_all()
            .map_err(|_| TranslateError::Io("failed to fsync temporary file".into()))?;
        drop(temp_file);

        atomic_replace_file(&temp, target)?;
        sync_parent_dir(parent)?;
        Ok(())
    })();

    if write_res.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_res
}

fn create_exclusive_private_file(path: &Path) -> Result<File, TranslateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| TranslateError::Io("failed to create exclusive 0600 temp file".into()))
    }
    #[cfg(windows)]
    {
        win_acl::create_exclusive_private_file_windows(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| TranslateError::Io("failed to create exclusive temp file".into()))
    }
}

fn atomic_replace_file(temp: &Path, target: &Path) -> Result<(), TranslateError> {
    #[cfg(windows)]
    {
        win_acl::replace_file_windows(temp, target)
    }
    #[cfg(not(windows))]
    {
        fs::rename(temp, target)
            .map_err(|_| TranslateError::Io("failed to atomically replace target file".into()))
    }
}

fn sync_parent_dir(parent: &Path) -> Result<(), TranslateError> {
    #[cfg(unix)]
    {
        let dir = File::open(parent)
            .map_err(|_| TranslateError::Io("failed to open parent directory for sync".into()))?;
        dir.sync_all()
            .map_err(|_| TranslateError::Io("failed to sync parent directory".into()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
    }
    Ok(())
}

/// Ensure a SQLite database file and its `-wal` / `-shm` / `-journal` sidecars are private
/// (`0600` on Unix, current-user-only ACL on Windows) and verify no-follow / owner / `nlink == 1`.
pub fn secure_sqlite_files(db_path: &Path) -> Result<(), TranslateError> {
    if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_private_dir(parent)?;
    }
    secure_single_sqlite_file(db_path, true)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = OsString::from(db_path.as_os_str());
        sidecar.push(suffix);
        secure_single_sqlite_file(Path::new(&sidecar), false)?;
    }
    Ok(())
}

fn secure_single_sqlite_file(path: &Path, create_if_missing: bool) -> Result<(), TranslateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        let file = if create_if_missing {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)
            {
                Ok(f) => f,
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(path)
                    .map_err(|e| {
                        if is_symlink_open_error(&e) {
                            TranslateError::Cache("cache file is a symlink".into())
                        } else {
                            TranslateError::Cache("failed to open cache file".into())
                        }
                    })?,
                Err(err) => {
                    return Err(TranslateError::Cache(format!(
                        "failed to create cache file: {}",
                        err.kind()
                    )));
                }
            }
        } else {
            match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(path)
            {
                Ok(f) => f,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(err) if is_symlink_open_error(&err) => {
                    return Err(TranslateError::Cache("cache sidecar is a symlink".into()));
                }
                Err(_) => {
                    return Err(TranslateError::Cache("failed to open cache sidecar".into()));
                }
            }
        };

        let meta = file
            .metadata()
            .map_err(|_| TranslateError::Cache("failed to stat cache file".into()))?;
        let euid = unsafe { libc::geteuid() };
        if !meta.is_file() || meta.uid() != euid || meta.nlink() != 1 {
            return Err(TranslateError::Cache(
                "cache file failed ownership or link-count security check".into(),
            ));
        }
        if meta.mode() & 0o777 != 0o600 {
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|_| TranslateError::Cache("failed to enforce 0600 on cache file".into()))?;
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        let file = if create_if_missing {
            match win_acl::create_exclusive_private_file_windows(path) {
                Ok(f) => f,
                Err(_) => OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(path)
                    .map_err(|_| TranslateError::Cache("failed to open cache file".into()))?,
            }
        } else {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
            {
                Ok(f) => f,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(_) => {
                    return Err(TranslateError::Cache("failed to open cache sidecar".into()));
                }
            }
        };
        win_acl::set_user_private_acl_on_handle(&file)?;
        win_acl::verify_windows_file_handle(&file)
            .map_err(|_| TranslateError::Cache("cache file failed Windows ACL check".into()))?;
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, create_if_missing);
        Ok(())
    }
}

fn is_symlink_open_error(err: &io::Error) -> bool {
    #[cfg(unix)]
    {
        if err.raw_os_error() == Some(libc::ELOOP) {
            return true;
        }
        // On some BSDs/macOS, O_NOFOLLOW on a symlink returns ELOOP; EMLINK on FreeBSD.
        #[cfg(target_os = "freebsd")]
        if err.raw_os_error() == Some(libc::EMLINK) {
            return true;
        }
    }
    let _ = err;
    false
}

#[cfg(windows)]
mod win_acl {
    use std::fs::File;
    use std::mem::{size_of, MaybeUninit};
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use std::path::Path;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetSecurityInfo, SetNamedSecurityInfoW, SetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACCESS_ALLOWED_ACE,
        ACCESS_ALLOWED_ACE_TYPE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION,
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSID, SECURITY_ATTRIBUTES, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, MoveFileExW, BY_HANDLE_FILE_INFORMATION,
        CREATE_NEW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use crate::error::TranslateError;

    struct CurrentUserSid {
        token_buf: Vec<u64>,
        sid_string: String,
    }

    impl CurrentUserSid {
        fn query() -> Result<Self, TranslateError> {
            let mut token: HANDLE = null_mut();
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
                return Err(TranslateError::Io("OpenProcessToken failed".into()));
            }
            let res = (|| {
                let mut required = 0u32;
                unsafe {
                    GetTokenInformation(token, TokenUser, null_mut(), 0, &mut required);
                }
                if required == 0 {
                    return Err(TranslateError::Io("GetTokenInformation size failed".into()));
                }
                let word_count = (required as usize).div_ceil(size_of::<u64>());
                let mut token_buf = vec![0u64; word_count];
                if unsafe {
                    GetTokenInformation(
                        token,
                        TokenUser,
                        token_buf.as_mut_ptr() as *mut _,
                        (token_buf.len() * size_of::<u64>()) as u32,
                        &mut required,
                    )
                } == 0
                {
                    return Err(TranslateError::Io("GetTokenInformation failed".into()));
                }
                let token_user = unsafe { &*(token_buf.as_ptr() as *const TOKEN_USER) };
                let mut sid_ptr = null_mut();
                if unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid_ptr) } == 0 {
                    return Err(TranslateError::Io("ConvertSidToStringSidW failed".into()));
                }
                let sid_string = unsafe {
                    let mut len = 0usize;
                    while *sid_ptr.add(len) != 0 {
                        len += 1;
                    }
                    String::from_utf16_lossy(std::slice::from_raw_parts(sid_ptr, len))
                };
                unsafe { LocalFree(sid_ptr as _) };
                Ok(Self {
                    token_buf,
                    sid_string,
                })
            })();
            unsafe { CloseHandle(token) };
            res
        }

        fn as_psid(&self) -> PSID {
            let token_user = unsafe { &*(self.token_buf.as_ptr() as *const TOKEN_USER) };
            token_user.User.Sid
        }
    }

    struct OwnedSecurityDescriptor {
        ptr: *mut std::ffi::c_void,
    }

    impl OwnedSecurityDescriptor {
        fn for_user(sid: &str) -> Result<Self, TranslateError> {
            let sddl = format!("O:{sid}G:{sid}D:P(A;;GA;;;{sid})");
            let mut sddl_w: Vec<u16> = sddl.encode_utf16().collect();
            sddl_w.push(0);
            let mut descriptor = null_mut();
            let mut size = 0u32;
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl_w.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    &mut size,
                )
            } == 0
                || descriptor.is_null()
            {
                return Err(TranslateError::Io(
                    "failed to build Windows user-private security descriptor".into(),
                ));
            }
            Ok(Self { ptr: descriptor })
        }

        fn dacl(&self) -> Result<*mut ACL, TranslateError> {
            let mut present = 0i32;
            let mut dacl: *mut ACL = null_mut();
            let mut defaulted = 0i32;
            if unsafe { GetSecurityDescriptorDacl(self.ptr, &mut present, &mut dacl, &mut defaulted) }
                == 0
                || present == 0
                || dacl.is_null()
            {
                return Err(TranslateError::Io(
                    "failed to extract DACL from security descriptor".into(),
                ));
            }
            Ok(dacl)
        }
    }

    impl Drop for OwnedSecurityDescriptor {
        fn drop(&mut self) {
            if !self.ptr.is_null() {
                unsafe { LocalFree(self.ptr as _) };
            }
        }
    }

    fn to_wide_null(path: &Path) -> Vec<u16> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        wide
    }

    pub fn set_user_private_acl_on_path(path: &Path) -> Result<(), TranslateError> {
        let user = CurrentUserSid::query()?;
        let sd = OwnedSecurityDescriptor::for_user(&user.sid_string)?;
        let dacl = sd.dacl()?;
        let mut wide = to_wide_null(path);
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide.as_mut_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                user.as_psid(),
                null_mut(),
                dacl,
                null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(TranslateError::Io(
                "failed to apply Windows user-private ACL on path".into(),
            ));
        }
        Ok(())
    }

    pub fn set_user_private_acl_on_handle(file: &File) -> Result<(), TranslateError> {
        let user = CurrentUserSid::query()?;
        let sd = OwnedSecurityDescriptor::for_user(&user.sid_string)?;
        let dacl = sd.dacl()?;
        let status = unsafe {
            SetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                user.as_psid(),
                null_mut(),
                dacl,
                null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(TranslateError::Io(
                "failed to apply Windows user-private ACL on handle".into(),
            ));
        }
        Ok(())
    }

    pub fn create_exclusive_private_file_windows(path: &Path) -> Result<File, TranslateError> {
        let user = CurrentUserSid::query()?;
        let sd = OwnedSecurityDescriptor::for_user(&user.sid_string)?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.ptr,
            bInheritHandle: 0,
        };
        let wide = to_wide_null(path);
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                0,
                &sa,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(TranslateError::Io(
                "failed to create exclusive Windows private file".into(),
            ));
        }
        Ok(unsafe { File::from_raw_handle(handle as _) })
    }

    pub fn replace_file_windows(temp: &Path, target: &Path) -> Result<(), TranslateError> {
        let temp_w = to_wide_null(temp);
        let target_w = to_wide_null(target);
        let ok = unsafe {
            MoveFileExW(
                temp_w.as_ptr(),
                target_w.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            return Err(TranslateError::Io(
                "MoveFileExW atomic replacement failed".into(),
            ));
        }
        Ok(())
    }

    pub fn verify_windows_file_handle(file: &File) -> Result<(), TranslateError> {
        let handle = file.as_raw_handle() as HANDLE;
        let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
        if unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) } == 0 {
            return Err(TranslateError::ConfigUnavailable(
                "GetFileInformationByHandle failed".into(),
            ));
        }
        let info = unsafe { info.assume_init() };
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
        {
            return Err(TranslateError::ConfigUnavailable(
                "file is a reparse point or directory".into(),
            ));
        }
        if info.nNumberOfLinks != 1 {
            return Err(TranslateError::ConfigUnavailable(
                "file must have exactly 1 hard link".into(),
            ));
        }

        let user = CurrentUserSid::query()?;
        let mut owner_sid: PSID = null_mut();
        let mut dacl: *mut ACL = null_mut();
        let mut sd = null_mut();
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner_sid,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut sd,
            )
        };
        if status != ERROR_SUCCESS || sd.is_null() {
            return Err(TranslateError::ConfigUnavailable(
                "GetSecurityInfo failed".into(),
            ));
        }

        let check_res = (|| -> Result<(), TranslateError> {
            if owner_sid.is_null() || unsafe { EqualSid(owner_sid, user.as_psid()) } == 0 {
                return Err(TranslateError::ConfigUnavailable(
                    "file is not owned by current user".into(),
                ));
            }
            let mut control = 0u16;
            let mut revision = 0u32;
            if unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) } == 0 {
                return Err(TranslateError::ConfigUnavailable(
                    "GetSecurityDescriptorControl failed".into(),
                ));
            }
            if control & (SE_DACL_PROTECTED as u16) == 0 {
                return Err(TranslateError::ConfigUnavailable(
                    "file DACL is not protected against inheritance".into(),
                ));
            }
            if dacl.is_null() {
                return Err(TranslateError::ConfigUnavailable(
                    "file has a null (world-accessible) DACL".into(),
                ));
            }
            let mut acl_size = MaybeUninit::<ACL_SIZE_INFORMATION>::zeroed();
            if unsafe {
                GetAclInformation(
                    dacl,
                    acl_size.as_mut_ptr() as *mut _,
                    size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            } == 0
            {
                return Err(TranslateError::ConfigUnavailable(
                    "GetAclInformation failed".into(),
                ));
            }
            let acl_size = unsafe { acl_size.assume_init() };
            if acl_size.AceCount == 0 {
                return Err(TranslateError::ConfigUnavailable("file DACL is empty".into()));
            }
            for idx in 0..acl_size.AceCount {
                let mut ace_ptr = null_mut();
                if unsafe { GetAce(dacl, idx, &mut ace_ptr) } == 0 || ace_ptr.is_null() {
                    return Err(TranslateError::ConfigUnavailable("GetAce failed".into()));
                }
                let header = unsafe { &*(ace_ptr as *const ACE_HEADER) };
                if header.AceType != ACCESS_ALLOWED_ACE_TYPE as u8 {
                    return Err(TranslateError::ConfigUnavailable(
                        "unexpected ACE type in private file DACL".into(),
                    ));
                }
                let allowed = unsafe { &*(ace_ptr as *const ACCESS_ALLOWED_ACE) };
                let ace_sid = &allowed.SidStart as *const u32 as PSID;
                if unsafe { EqualSid(ace_sid, user.as_psid()) } == 0 {
                    return Err(TranslateError::ConfigUnavailable(
                        "file DACL grants access to another principal".into(),
                    ));
                }
            }
            let _ = null::<u8>();
            Ok(())
        })();

        unsafe { LocalFree(sd as _) };
        check_res
    }
}
