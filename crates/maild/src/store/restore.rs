use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use super::backup::{
    create_verified_snapshot, validate_integrity, BackupError, SnapshotValidation,
};
use super::lock::{normalize_database_path, DatabaseLock, DatabaseLockError};
use super::{backup_database, migrations, StoreOpenError};

const RESTORE_TEMP_PREFIX: &str = ".maild-restore-";

#[derive(Debug)]
pub enum RestoreError {
    InvalidDatabase(PathBuf),
    InvalidInput(PathBuf),
    InvalidRollback(PathBuf),
    ConflictingPaths,
    Lock(DatabaseLockError),
    Backup(BackupError),
    Database(rusqlite::Error),
    Schema(StoreOpenError),
    UnexpectedJournalMode(String),
    AfterRollback {
        rollback: PathBuf,
        source: Box<RestoreError>,
    },
    PublishedButDirectorySyncFailed {
        database: PathBuf,
        rollback: PathBuf,
        error: std::io::Error,
    },
    Io(std::io::Error),
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDatabase(path) => write!(
                f,
                "待恢复数据库必须是现有普通文件，且不能是符号链接或硬链接: {path:?}"
            ),
            Self::InvalidInput(path) => {
                write!(f, "恢复输入必须是现有普通 SQLite 文件且不能是符号链接: {path:?}")
            }
            Self::InvalidRollback(path) => write!(
                f,
                "回滚快照必须使用尚不存在、父目录已存在的文件路径: {path:?}"
            ),
            Self::ConflictingPaths => write!(f, "database、input 与 rollback 必须是三个不同路径"),
            Self::Lock(error) => write!(f, "无法进入维护模式: {error}"),
            Self::Backup(error) => write!(f, "创建恢复快照或回滚快照失败: {error}"),
            Self::Database(error) => write!(f, "SQLite 恢复错误: {error}"),
            Self::Schema(error) => write!(f, "数据库 schema 校验失败: {error}"),
            Self::UnexpectedJournalMode(mode) => {
                write!(f, "数据库无法切换到 DELETE journal mode，实际为 {mode:?}")
            }
            Self::AfterRollback { rollback, source } => write!(
                f,
                "回滚快照已保存在 {rollback:?}，但恢复发布前失败；原数据库仍可用: {source}"
            ),
            Self::PublishedButDirectorySyncFailed {
                database,
                rollback,
                error,
            } => write!(
                f,
                "新数据库已发布到 {database:?}，回滚快照保存在 {rollback:?}，但目录同步失败，掉电持久性未确认: {error}"
            ),
            Self::Io(error) => write!(f, "恢复文件系统错误: {error}"),
        }
    }
}

impl Error for RestoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Lock(error) => Some(error),
            Self::Backup(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::Schema(error) => Some(error),
            Self::AfterRollback { source, .. } => Some(source),
            Self::PublishedButDirectorySyncFailed { error, .. } | Self::Io(error) => Some(error),
            Self::InvalidDatabase(_)
            | Self::InvalidInput(_)
            | Self::InvalidRollback(_)
            | Self::ConflictingPaths
            | Self::UnexpectedJournalMode(_) => None,
        }
    }
}

impl From<DatabaseLockError> for RestoreError {
    fn from(error: DatabaseLockError) -> Self {
        Self::Lock(error)
    }
}

impl From<BackupError> for RestoreError {
    fn from(error: BackupError) -> Self {
        Self::Backup(error)
    }
}

impl From<rusqlite::Error> for RestoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<StoreOpenError> for RestoreError {
    fn from(error: StoreOpenError) -> Self {
        Self::Schema(error)
    }
}

impl From<std::io::Error> for RestoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct RestoreOutcome {
    pub database: PathBuf,
    pub rollback: PathBuf,
}

/// Replace a stopped maild database with a verified backup while retaining a verified rollback.
///
/// The database lock is acquired before either database is inspected and held until the atomic
/// replacement and directory sync have completed. The rollback destination must not exist.
pub fn restore_database(
    database: &Path,
    input: &Path,
    rollback: &Path,
) -> Result<RestoreOutcome, RestoreError> {
    validate_current_database_path(database)?;
    let lock = DatabaseLock::acquire(database)?;
    let database = lock.database_path().to_path_buf();
    let input = normalize_existing_input(input)?;
    let rollback = normalize_new_rollback(rollback)?;
    let sqlite_sidecars = sqlite_sidecar_paths(&database);
    if paths_conflict(&database, &input)
        || paths_conflict(&database, &rollback)
        || paths_conflict(&input, &rollback)
        || sqlite_sidecars
            .iter()
            .any(|sidecar| paths_conflict(sidecar, &input) || paths_conflict(sidecar, &rollback))
        || is_reserved_sidecar_location(&database, &input)
        || is_reserved_sidecar_location(&database, &rollback)
    {
        return Err(RestoreError::ConflictingPaths);
    }

    validate_existing_database(&database)?;
    validate_restore_input_database(&input)?;

    let parent = database
        .parent()
        .ok_or_else(|| RestoreError::InvalidDatabase(database.clone()))?;
    #[cfg(unix)]
    let parent_directory = fs::File::open(parent)?;
    let replacement = create_verified_snapshot(
        &input,
        parent,
        RESTORE_TEMP_PREFIX,
        SnapshotValidation::RestoreInput,
    )?;

    // ADR-0022：历史备份只在临时 replacement 上迁移。迁移、当前 schema、
    // 完整性与文件同步全部通过后，才允许创建 rollback 并原子发布；任何
    // 畸形 v1/v2 输入都不能先覆盖正式数据库。
    {
        let mut candidate = Connection::open(replacement.path())?;
        migrations::prepare(&mut candidate)?;
        let (busy, _, _): (i64, i64, i64) =
            candidate.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        if busy != 0 {
            return Err(RestoreError::Database(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("恢复候选仍有活动 SQLite 连接".into()),
            )));
        }
        let mode = candidate.query_row("PRAGMA journal_mode=DELETE", [], |row| {
            row.get::<_, String>(0)
        })?;
        if !mode.eq_ignore_ascii_case("delete") {
            return Err(RestoreError::UnexpectedJournalMode(mode));
        }
        migrations::validate_existing(&candidate)?;
        validate_integrity(&candidate)?;
    }
    replacement.as_file().sync_all()?;
    backup_database(&database, &rollback)?;

    let publish_result = (|| -> Result<(), RestoreError> {
        quiesce_current_database(&database)?;
        remove_sqlite_sidecars(&database)?;
        replacement
            .persist(&database)
            .map_err(|error| RestoreError::Io(error.error))?;
        Ok(())
    })();
    if let Err(source) = publish_result {
        return Err(RestoreError::AfterRollback {
            rollback,
            source: Box::new(source),
        });
    }

    #[cfg(unix)]
    parent_directory
        .sync_all()
        .map_err(|error| RestoreError::PublishedButDirectorySyncFailed {
            database: database.clone(),
            rollback: rollback.clone(),
            error,
        })?;

    Ok(RestoreOutcome { database, rollback })
}

fn validate_current_database_path(path: &Path) -> Result<(), RestoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| RestoreError::InvalidDatabase(path.to_path_buf()))?;
    if !metadata.file_type().is_file() {
        return Err(RestoreError::InvalidDatabase(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Err(RestoreError::InvalidDatabase(path.to_path_buf()));
        }
    }
    Ok(())
}

fn normalize_existing_input(path: &Path) -> Result<PathBuf, RestoreError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| RestoreError::InvalidInput(path.to_path_buf()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(RestoreError::InvalidInput(path.to_path_buf()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(RestoreError::InvalidInput(path.to_path_buf()));
        }
    }
    fs::canonicalize(path).map_err(RestoreError::Io)
}

fn normalize_new_rollback(path: &Path) -> Result<PathBuf, RestoreError> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(RestoreError::InvalidRollback(path.to_path_buf()));
    }
    match normalize_database_path(path) {
        Ok(path) => Ok(path),
        Err(
            DatabaseLockError::Io(_)
            | DatabaseLockError::InvalidDatabasePath(_)
            | DatabaseLockError::UnsafeDatabaseDirectory(_)
            | DatabaseLockError::UnsafeDatabaseFile(_)
            | DatabaseLockError::UnsafeLockFile(_)
            | DatabaseLockError::InUse { .. },
        ) => Err(RestoreError::InvalidRollback(path.to_path_buf())),
    }
}

fn validate_existing_database(path: &Path) -> Result<(), RestoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    migrations::validate_existing(&connection)?;
    validate_integrity(&connection)?;
    Ok(())
}

/// 恢复输入接受可迁移的历史版本（ADR-0022），但每个版本仍须先通过
/// 自己的精确结构与完整性校验；迁移只发生在临时 replacement 上。
fn validate_restore_input_database(path: &Path) -> Result<(), RestoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    migrations::validate_restore_input(&connection)?;
    validate_integrity(&connection)?;
    Ok(())
}

fn quiesce_current_database(path: &Path) -> Result<(), RestoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    migrations::validate_existing(&connection)?;
    validate_integrity(&connection)?;
    let (busy, _, _): (i64, i64, i64) =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if busy != 0 {
        return Err(RestoreError::Database(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("旧数据库仍有活动 SQLite 连接".into()),
        )));
    }
    let mode = connection.query_row("PRAGMA journal_mode=DELETE", [], |row| {
        row.get::<_, String>(0)
    })?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(RestoreError::UnexpectedJournalMode(mode));
    }
    drop(connection);
    Ok(())
}

fn remove_sqlite_sidecars(path: &Path) -> Result<(), RestoreError> {
    for sidecar in sqlite_sidecar_paths(path).into_iter().take(2) {
        match fs::remove_file(sidecar) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn sqlite_sidecar_paths(path: &Path) -> [PathBuf; 3] {
    ["-wal", "-shm", "-journal"].map(|suffix| {
        let mut sidecar = OsString::from(path.as_os_str());
        sidecar.push(suffix);
        PathBuf::from(sidecar)
    })
}

fn paths_conflict(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        false
    }
}

fn is_reserved_sidecar_location(database: &Path, candidate: &Path) -> bool {
    if database.parent() != candidate.parent() {
        return false;
    }
    let Some(file_name) = candidate.file_name() else {
        return false;
    };
    let file_name = file_name.to_string_lossy();
    ["-wal", "-shm", "-journal"]
        .iter()
        .any(|suffix| ends_with_ascii_case(file_name.as_bytes(), suffix.as_bytes()))
}

fn ends_with_ascii_case(value: &[u8], suffix: &[u8]) -> bool {
    value.len() >= suffix.len()
        && value[value.len() - suffix.len()..]
            .iter()
            .zip(suffix)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}
