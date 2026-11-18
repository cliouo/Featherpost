use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};
use tempfile::{Builder, NamedTempFile};

use super::migrations;
use super::StoreOpenError;

const PAGES_PER_STEP: i32 = 100;
const STEP_PAUSE: Duration = Duration::from_millis(10);
const LOCKED_TIMEOUT: Duration = Duration::from_secs(30);
const TEMP_PREFIX: &str = ".maild-backup-";
const MAX_DIAGNOSTIC_CHARS: usize = 2_048;
const MAX_INTEGRITY_ERRORS: usize = 10;

#[derive(Debug)]
pub enum BackupError {
    InvalidSource(PathBuf),
    InvalidDestination(PathBuf),
    DestinationExists(PathBuf),
    UnsafeDestinationDirectory(PathBuf),
    LockedTimeout,
    IntegrityCheck(Vec<String>),
    PublishedButDirectorySyncFailed {
        destination: PathBuf,
        error: std::io::Error,
    },
    Io(std::io::Error),
    Database(rusqlite::Error),
    Schema(StoreOpenError),
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSource(path) => {
                write!(f, "源数据库不是现有普通文件: {path:?}")
            }
            Self::InvalidDestination(path) => {
                write!(f, "备份目标必须包含有效文件名: {path:?}")
            }
            Self::DestinationExists(path) => {
                write!(f, "备份目标已存在，不会覆盖: {path:?}")
            }
            Self::UnsafeDestinationDirectory(path) => write!(
                f,
                "Unix 规范化备份目录必须由当前用户拥有，且不可被组或其他用户写入: {path:?}"
            ),
            Self::LockedTimeout => write!(f, "源数据库连续锁定 30 秒，备份已取消"),
            Self::IntegrityCheck(details) => {
                write!(
                    f,
                    "备份数据库完整性校验失败: {}",
                    bounded_integrity_details(details)
                )
            }
            Self::PublishedButDirectorySyncFailed { destination, error } => write!(
                f,
                "备份已发布到 {destination:?}，但目标目录同步失败，掉电持久性未确认: {}",
                bounded_display(error)
            ),
            Self::Io(error) => write!(f, "文件系统错误: {}", bounded_display(error)),
            Self::Database(error) => write!(f, "SQLite 备份错误: {}", bounded_display(error)),
            Self::Schema(error) => write!(f, "数据库 schema 校验失败: {}", bounded_display(error)),
        }
    }
}

impl Error for BackupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::Schema(error) => Some(error),
            Self::PublishedButDirectorySyncFailed { error, .. } => Some(error),
            Self::InvalidSource(_)
            | Self::InvalidDestination(_)
            | Self::DestinationExists(_)
            | Self::UnsafeDestinationDirectory(_)
            | Self::LockedTimeout
            | Self::IntegrityCheck(_) => None,
        }
    }
}

impl From<std::io::Error> for BackupError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for BackupError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<StoreOpenError> for BackupError {
    fn from(error: StoreOpenError) -> Self {
        Self::Schema(error)
    }
}

/// Create a verified SQLite snapshot without mutating or exclusively locking the source store.
///
/// The destination must not exist. A temporary file in the destination directory is only
/// published after the online backup, schema validation, integrity check, and file sync succeed.
pub fn backup_database(source: &Path, destination: &Path) -> Result<(), BackupError> {
    if !fs::metadata(source)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
    {
        return Err(BackupError::InvalidSource(source.to_path_buf()));
    }
    let file_name = destination
        .file_name()
        .ok_or_else(|| BackupError::InvalidDestination(destination.to_path_buf()))?;
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !fs::metadata(parent)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
    {
        return Err(BackupError::InvalidDestination(destination.to_path_buf()));
    }
    let canonical_parent = fs::canonicalize(parent)?;
    let published_destination = canonical_parent.join(file_name);
    match fs::symlink_metadata(&published_destination) {
        Ok(_) => return Err(BackupError::DestinationExists(destination.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    validate_unix_destination_directory(&canonical_parent)?;
    #[cfg(unix)]
    let parent_directory = fs::File::open(&canonical_parent)?;

    let temporary = create_verified_snapshot(
        source,
        &canonical_parent,
        TEMP_PREFIX,
        SnapshotValidation::Current,
    )?;
    match temporary.persist_noclobber(&published_destination) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(BackupError::DestinationExists(destination.to_path_buf()));
        }
        Err(error) => return Err(BackupError::Io(error.error)),
    }
    #[cfg(unix)]
    parent_directory
        .sync_all()
        .map_err(|error| BackupError::PublishedButDirectorySyncFailed {
            destination: destination.to_path_buf(),
            error,
        })?;
    Ok(())
}

/// 快照源的结构校验强度：`Current` 要求恰好当前版本（常规备份）；
/// `RestoreInput` 接受可迁移的历史版本（ADR-0022：旧备份永远可进新库，
/// 由恢复调用方在临时 replacement 上 `prepare` 后再发布）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SnapshotValidation {
    Current,
    RestoreInput,
}

fn validate_schema_for(
    connection: &Connection,
    mode: SnapshotValidation,
) -> Result<(), StoreOpenError> {
    match mode {
        SnapshotValidation::Current => migrations::validate_existing(connection),
        SnapshotValidation::RestoreInput => migrations::validate_restore_input(connection),
    }
}

pub(super) fn create_verified_snapshot(
    source: &Path,
    destination_directory: &Path,
    prefix: &str,
    mode: SnapshotValidation,
) -> Result<NamedTempFile, BackupError> {
    if !fs::metadata(source)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
    {
        return Err(BackupError::InvalidSource(source.to_path_buf()));
    }
    let source_connection = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    validate_schema_for(&source_connection, mode)?;
    validate_integrity(&source_connection)?;

    let temporary = Builder::new()
        .prefix(prefix)
        .suffix(".sqlite3.tmp")
        .tempfile_in(destination_directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o600))?;
    }

    let mut destination_connection = Connection::open(temporary.path())?;
    copy_online(&source_connection, &mut destination_connection)?;
    validate_schema_for(&destination_connection, mode)?;
    validate_integrity(&destination_connection)?;
    drop(destination_connection);

    temporary.as_file().sync_all()?;
    Ok(temporary)
}

#[cfg(unix)]
fn validate_unix_destination_directory(parent: &Path) -> Result<(), BackupError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let leaf = fs::symlink_metadata(parent)?;
    let euid = unsafe { libc::geteuid() } as u32;
    if leaf.file_type().is_symlink()
        || !leaf.file_type().is_dir()
        || leaf.uid() != euid
        || leaf.permissions().mode() & 0o022 != 0
    {
        return Err(BackupError::UnsafeDestinationDirectory(
            parent.to_path_buf(),
        ));
    }

    let canonical_parent = fs::canonicalize(parent)?;
    let mut ancestor = canonical_parent.as_path();
    loop {
        let metadata = fs::symlink_metadata(ancestor)?;
        let mode = metadata.permissions().mode();
        let trusted_owner = metadata.uid() == euid || metadata.uid() == 0;
        let public_writable = mode & 0o022 != 0;
        let sticky_root = mode & 0o1000 != 0 && metadata.uid() == 0;
        if !metadata.file_type().is_dir() || !trusted_owner || (public_writable && !sticky_root) {
            return Err(BackupError::UnsafeDestinationDirectory(
                parent.to_path_buf(),
            ));
        }
        if ancestor == Path::new("/") {
            break;
        }
        ancestor = ancestor
            .parent()
            .ok_or_else(|| BackupError::UnsafeDestinationDirectory(parent.to_path_buf()))?;
    }
    Ok(())
}

fn copy_online(source: &Connection, destination: &mut Connection) -> Result<(), BackupError> {
    let backup = Backup::new(source, destination)?;
    let mut locked_since = None;
    loop {
        match backup.step(PAGES_PER_STEP)? {
            StepResult::Done => return Ok(()),
            StepResult::More => locked_since = None,
            StepResult::Busy | StepResult::Locked => {
                let since = locked_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= LOCKED_TIMEOUT {
                    return Err(BackupError::LockedTimeout);
                }
            }
            _ => unreachable!("rusqlite returned an unknown backup step result"),
        }
        thread::sleep(STEP_PAUSE);
    }
}

pub(super) fn validate_integrity(connection: &Connection) -> Result<(), BackupError> {
    let mut statement = connection.prepare("PRAGMA quick_check(10)")?;
    let results = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if results.as_slice() == ["ok"] {
        Ok(())
    } else {
        Err(BackupError::IntegrityCheck(results))
    }
}

fn bounded_integrity_details(details: &[String]) -> String {
    use fmt::Write as _;

    let mut output = BoundedText::new();
    for (index, detail) in details.iter().take(MAX_INTEGRITY_ERRORS).enumerate() {
        if index > 0 {
            let _ = output.write_str("; ");
        }
        let _ = output.write_str(detail);
    }
    if details.len() > MAX_INTEGRITY_ERRORS {
        let _ = output.write_str("; …");
    }
    output.finish()
}

fn bounded_display(value: &dyn fmt::Display) -> String {
    use fmt::Write as _;

    let mut output = BoundedText::new();
    let _ = write!(&mut output, "{value}");
    output.finish()
}

struct BoundedText {
    output: String,
    chars: usize,
    truncated: bool,
}

impl BoundedText {
    fn new() -> Self {
        Self {
            output: String::with_capacity(MAX_DIAGNOSTIC_CHARS),
            chars: 0,
            truncated: false,
        }
    }

    fn finish(mut self) -> String {
        if self.truncated {
            self.output.push('…');
        }
        self.output
    }
}

impl fmt::Write for BoundedText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for character in value.chars() {
            if self.chars == MAX_DIAGNOSTIC_CHARS {
                self.truncated = true;
                break;
            }
            self.output.push(if character.is_control() {
                ' '
            } else {
                character
            });
            self.chars += 1;
        }
        Ok(())
    }
}
