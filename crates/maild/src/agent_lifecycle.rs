//! Fixed lifecycle lock shared by the platform Agent and macOS maintenance tooling.
//!
//! The normal Agent creates and holds the lock before reading configuration and releases it only
//! after daemon teardown. The maintenance probe is deliberately read-only: a missing lock means
//! idle, and probing never creates the runtime directory or lock file.

use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

const LIFECYCLE_LOCK_NAME: &str = "maild-agent.lifecycle.lock";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentLifecycleStatus {
    Idle,
    Busy,
}

#[derive(Debug)]
pub enum AgentLifecycleError {
    InvalidRuntimeDirectory(PathBuf),
    UnsafeRuntimeDirectory(PathBuf),
    UnsafeLockFile(PathBuf),
    InUse(PathBuf),
    Io(io::Error),
}

impl fmt::Display for AgentLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRuntimeDirectory(path) => {
                write!(formatter, "Agent runtime 目录必须是绝对路径: {path:?}")
            }
            Self::UnsafeRuntimeDirectory(path) => write!(
                formatter,
                "Agent runtime 目录必须是当前用户拥有的非符号链接私有目录: {path:?}"
            ),
            Self::UnsafeLockFile(path) => write!(
                formatter,
                "Agent 生命周期锁必须是当前用户拥有、单链接且权限不宽于 0600 的普通文件: {path:?}"
            ),
            Self::InUse(path) => write!(formatter, "Agent 生命周期锁正在被持有: {path:?}"),
            Self::Io(error) => write!(formatter, "Agent 生命周期锁文件系统错误: {error}"),
        }
    }
}

impl Error for AgentLifecycleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidRuntimeDirectory(_)
            | Self::UnsafeRuntimeDirectory(_)
            | Self::UnsafeLockFile(_)
            | Self::InUse(_) => None,
        }
    }
}

impl From<io::Error> for AgentLifecycleError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// An exclusive lease held for the complete lifetime of one platform Agent process.
pub struct AgentLifecycleLock {
    file: File,
    path: PathBuf,
}

impl AgentLifecycleLock {
    pub fn acquire(
        managed_root: &Path,
        runtime_directory: &Path,
    ) -> Result<Self, AgentLifecycleError> {
        validate_managed_paths(managed_root, runtime_directory)?;
        ensure_private_runtime_directory(managed_root, runtime_directory)?;
        let path = lock_path(runtime_directory);
        let file = open_lock(&path, true)?;
        validate_lock_file(&file, &path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file, path }),
            Err(std::fs::TryLockError::WouldBlock) => Err(AgentLifecycleError::InUse(path)),
            Err(std::fs::TryLockError::Error(error)) => Err(AgentLifecycleError::Io(error)),
        }
    }

    /// Probe without creating the runtime directory or persistent lock inode.
    pub fn probe(
        managed_root: &Path,
        runtime_directory: &Path,
    ) -> Result<AgentLifecycleStatus, AgentLifecycleError> {
        validate_managed_paths(managed_root, runtime_directory)?;
        if !validate_managed_directory_read_only(managed_root, runtime_directory)? {
            return Ok(AgentLifecycleStatus::Idle);
        }

        let path = lock_path(runtime_directory);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                    return Err(AgentLifecycleError::UnsafeLockFile(path));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(AgentLifecycleStatus::Idle);
            }
            Err(error) => return Err(AgentLifecycleError::Io(error)),
        }

        let file = open_lock(&path, false)?;
        validate_lock_file(&file, &path)?;
        match file.try_lock() {
            Ok(()) => {
                let _ = file.unlock();
                Ok(AgentLifecycleStatus::Idle)
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(AgentLifecycleStatus::Busy),
            Err(std::fs::TryLockError::Error(error)) => Err(AgentLifecycleError::Io(error)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the already-held lock descriptor open across an `exec` boundary.
    ///
    /// The caller must retain `self` until `exec` succeeds. If `exec` fails, normal drop releases
    /// the lease. The replacement process owns the inherited descriptor until it exits.
    #[cfg(unix)]
    pub fn make_inheritable(&self) -> Result<std::os::fd::RawFd, AgentLifecycleError> {
        use std::os::fd::AsRawFd as _;

        let descriptor = self.file.as_raw_fd();
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        if flags == -1 {
            return Err(AgentLifecycleError::Io(io::Error::last_os_error()));
        }
        if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } == -1 {
            return Err(AgentLifecycleError::Io(io::Error::last_os_error()));
        }
        Ok(descriptor)
    }

    /// Validate that an inherited descriptor names the fixed lifecycle lock and holds (or can
    /// safely establish) its exclusive lease. This is used only by the exec'd maintenance shell.
    #[cfg(unix)]
    pub fn validate_inherited(
        managed_root: &Path,
        runtime_directory: &Path,
        descriptor: std::os::fd::RawFd,
    ) -> Result<(), AgentLifecycleError> {
        use std::os::fd::FromRawFd as _;
        use std::os::unix::fs::MetadataExt as _;

        validate_managed_paths(managed_root, runtime_directory)?;
        if !validate_managed_directory_read_only(managed_root, runtime_directory)? {
            return Err(AgentLifecycleError::UnsafeRuntimeDirectory(
                runtime_directory.to_path_buf(),
            ));
        }

        let path = lock_path(runtime_directory);
        let path_metadata = fs::symlink_metadata(&path).map_err(AgentLifecycleError::Io)?;
        if !path_metadata.file_type().is_file() || path_metadata.file_type().is_symlink() {
            return Err(AgentLifecycleError::UnsafeLockFile(path));
        }

        // SAFETY: the maintenance parent explicitly inherited this descriptor. This validator is
        // a short-lived child, so consuming and closing its copy cannot close the parent's copy.
        let file = unsafe { File::from_raw_fd(descriptor) };
        validate_lock_file(&file, &path)?;
        let descriptor_metadata = file.metadata().map_err(AgentLifecycleError::Io)?;
        if descriptor_metadata.dev() != path_metadata.dev()
            || descriptor_metadata.ino() != path_metadata.ino()
        {
            return Err(AgentLifecycleError::UnsafeLockFile(path));
        }
        match file.try_lock() {
            Ok(()) => Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => Err(AgentLifecycleError::InUse(path)),
            Err(std::fs::TryLockError::Error(error)) => Err(AgentLifecycleError::Io(error)),
        }
    }
}

impl Drop for AgentLifecycleLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn validate_managed_paths(
    managed_root: &Path,
    runtime_directory: &Path,
) -> Result<(), AgentLifecycleError> {
    if !managed_root.is_absolute()
        || !runtime_directory.is_absolute()
        || runtime_directory.strip_prefix(managed_root).is_err()
    {
        return Err(AgentLifecycleError::InvalidRuntimeDirectory(
            runtime_directory.to_path_buf(),
        ));
    }
    Ok(())
}

fn lock_path(runtime_directory: &Path) -> PathBuf {
    runtime_directory.join(LIFECYCLE_LOCK_NAME)
}

fn ensure_private_runtime_directory(
    managed_root: &Path,
    runtime_directory: &Path,
) -> Result<(), AgentLifecycleError> {
    ensure_private_root(managed_root)?;
    let relative = runtime_directory.strip_prefix(managed_root).map_err(|_| {
        AgentLifecycleError::InvalidRuntimeDirectory(runtime_directory.to_path_buf())
    })?;
    let mut current = managed_root.to_path_buf();
    let component_count = relative.components().count();
    if component_count == 0 {
        return Ok(());
    }
    for (index, component) in relative.components().enumerate() {
        current.push(component);
        let final_component = index + 1 == component_count;
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if final_component {
                    validate_runtime_directory(&current, &metadata)?;
                } else {
                    validate_runtime_ancestor(&current, &metadata)?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt as _;
                    let mut builder = fs::DirBuilder::new();
                    builder.mode(0o700);
                    builder.create(&current).map_err(AgentLifecycleError::Io)?;
                }
                #[cfg(not(unix))]
                fs::create_dir(&current).map_err(AgentLifecycleError::Io)?;
                let metadata = fs::symlink_metadata(&current).map_err(AgentLifecycleError::Io)?;
                validate_runtime_directory(&current, &metadata)?;
            }
            Err(error) => return Err(AgentLifecycleError::Io(error)),
        }
    }
    Ok(())
}

fn ensure_private_root(path: &Path) -> Result<(), AgentLifecycleError> {
    validate_existing_ancestor_chain(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => return validate_runtime_directory(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(AgentLifecycleError::Io(error)),
    }

    let mut missing = Vec::new();
    let mut ancestor = path;
    loop {
        missing.push(
            ancestor
                .file_name()
                .ok_or_else(|| AgentLifecycleError::InvalidRuntimeDirectory(path.to_path_buf()))?
                .to_os_string(),
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| AgentLifecycleError::InvalidRuntimeDirectory(path.to_path_buf()))?;
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                validate_runtime_ancestor(ancestor, &metadata)?;
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(AgentLifecycleError::Io(error)),
        }
    }

    let mut current = ancestor.to_path_buf();
    for component in missing.into_iter().rev() {
        current.push(component);
        create_private_directory(&current)?;
    }
    let metadata = fs::symlink_metadata(path).map_err(AgentLifecycleError::Io)?;
    validate_runtime_directory(path, &metadata)
}

/// Validate every existing lexical ancestor above the managed root through `/`.
///
/// The managed root itself has the stricter current-user/private rule; its parents may be
/// current-user or root owned, but writable parents are accepted only when root-owned and sticky.
fn validate_existing_ancestor_chain(path: &Path) -> Result<(), AgentLifecycleError> {
    let mut current = path.parent();
    while let Some(ancestor) = current {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => validate_runtime_ancestor(ancestor, &metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(AgentLifecycleError::Io(error)),
        }
        current = ancestor.parent();
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), AgentLifecycleError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path).map_err(AgentLifecycleError::Io)?;
    }
    #[cfg(not(unix))]
    fs::create_dir(path).map_err(AgentLifecycleError::Io)?;
    let metadata = fs::symlink_metadata(path).map_err(AgentLifecycleError::Io)?;
    validate_runtime_directory(path, &metadata)
}

fn validate_managed_directory_read_only(
    managed_root: &Path,
    runtime_directory: &Path,
) -> Result<bool, AgentLifecycleError> {
    validate_existing_ancestor_chain(managed_root)?;
    let root_metadata = match fs::symlink_metadata(managed_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(AgentLifecycleError::Io(error)),
    };
    validate_runtime_directory(managed_root, &root_metadata)?;
    let relative = runtime_directory.strip_prefix(managed_root).map_err(|_| {
        AgentLifecycleError::InvalidRuntimeDirectory(runtime_directory.to_path_buf())
    })?;
    let mut current = managed_root.to_path_buf();
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(AgentLifecycleError::Io(error)),
        };
        if index + 1 == component_count {
            validate_runtime_directory(&current, &metadata)?;
        } else {
            validate_runtime_ancestor(&current, &metadata)?;
        }
    }
    Ok(true)
}

fn validate_runtime_ancestor(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), AgentLifecycleError> {
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AgentLifecycleError::UnsafeRuntimeDirectory(
            path.to_path_buf(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let current_uid = unsafe { libc::geteuid() };
        let writable = metadata.mode() & 0o022 != 0;
        let root_sticky = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        if (metadata.uid() != 0 && metadata.uid() != current_uid) || (writable && !root_sticky) {
            return Err(AgentLifecycleError::UnsafeRuntimeDirectory(
                path.to_path_buf(),
            ));
        }
    }
    Ok(())
}

fn validate_runtime_directory(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), AgentLifecycleError> {
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AgentLifecycleError::UnsafeRuntimeDirectory(
            path.to_path_buf(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(AgentLifecycleError::UnsafeRuntimeDirectory(
                path.to_path_buf(),
            ));
        }
    }
    Ok(())
}

fn open_lock(path: &Path, create: bool) -> Result<File, AgentLifecycleError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|error| {
        #[cfg(unix)]
        if error.raw_os_error() == Some(libc::ELOOP) {
            return AgentLifecycleError::UnsafeLockFile(path.to_path_buf());
        }
        AgentLifecycleError::Io(error)
    })
}

fn validate_lock_file(file: &File, path: &Path) -> Result<(), AgentLifecycleError> {
    let metadata = file.metadata().map_err(AgentLifecycleError::Io)?;
    if !metadata.is_file() {
        return Err(AgentLifecycleError::UnsafeLockFile(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(AgentLifecycleError::UnsafeLockFile(path.to_path_buf()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_is_read_only_and_lock_tracks_owner_lifetime() {
        let directory = tempfile::tempdir().unwrap();
        let directory = fs::canonicalize(directory.path()).unwrap();
        let root = directory.join("data");
        let runtime = root.join("run");
        assert_eq!(
            AgentLifecycleLock::probe(&root, &runtime).unwrap(),
            AgentLifecycleStatus::Idle
        );
        assert!(!root.exists());

        let owner = AgentLifecycleLock::acquire(&root, &runtime).unwrap();
        assert!(owner.path().exists());
        assert!(matches!(
            AgentLifecycleLock::acquire(&root, &runtime),
            Err(AgentLifecycleError::InUse(_))
        ));
        assert_eq!(
            AgentLifecycleLock::probe(&root, &runtime).unwrap(),
            AgentLifecycleStatus::Busy
        );
        drop(owner);
        assert_eq!(
            AgentLifecycleLock::probe(&root, &runtime).unwrap(),
            AgentLifecycleStatus::Idle
        );
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_runtime_and_lock_entries_fail_closed() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let directory = tempfile::tempdir().unwrap();
        let directory = fs::canonicalize(directory.path()).unwrap();
        let root = directory.join("data");
        let runtime = root.join("run");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            AgentLifecycleLock::probe(&root, &runtime),
            Err(AgentLifecycleError::UnsafeRuntimeDirectory(_))
        ));

        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let target = directory.join("target");
        fs::write(&target, b"not a lock").unwrap();
        symlink(&target, lock_path(&runtime)).unwrap();
        assert!(matches!(
            AgentLifecycleLock::probe(&root, &runtime),
            Err(AgentLifecycleError::UnsafeLockFile(_))
        ));

        fs::remove_file(lock_path(&runtime)).unwrap();
        fs::remove_dir(&runtime).unwrap();
        symlink(&target, &runtime).unwrap();
        assert!(matches!(
            AgentLifecycleLock::acquire(&root, &runtime),
            Err(AgentLifecycleError::UnsafeRuntimeDirectory(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn non_sticky_writable_ancestor_fails_closed() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let directory = fs::canonicalize(directory.path()).unwrap();
        let unsafe_ancestor = directory.join("unsafe");
        fs::create_dir(&unsafe_ancestor).unwrap();
        fs::set_permissions(&unsafe_ancestor, fs::Permissions::from_mode(0o777)).unwrap();
        let root = unsafe_ancestor.join("data");
        let runtime = root.join("run");

        assert!(matches!(
            AgentLifecycleLock::acquire(&root, &runtime),
            Err(AgentLifecycleError::UnsafeRuntimeDirectory(path)) if path == unsafe_ancestor
        ));
        assert!(!root.exists());
    }
}
