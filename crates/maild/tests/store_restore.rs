use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use maild::events::MailMessage;
use maild::store::{
    restore_database, DatabaseLockError, RestoreError, Store, StoreOpenError, SCHEMA_VERSION,
};
use rusqlite::{Connection, OpenFlags};

fn message(account: &str, uid: &str, body: &str) -> MailMessage {
    MailMessage {
        account: account.into(),
        uid: uid.into(),
        sender: "sender@example.test".into(),
        recipient: String::new(),
        subject: "restore snapshot".into(),
        body: body.into(),
        received_at: 42,
        ..Default::default()
    }
}

fn create_store(path: &Path, account: &str, uid: &str, body: &str) {
    let store = Store::open(path.to_str().unwrap()).unwrap();
    assert!(store.insert_message(&message(account, uid, body)).unwrap());
    assert!(store.mark_notified(account, uid).unwrap());
}

fn assert_database(path: &Path, account: &str, uid: &str, body: &str) {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    assert_eq!(
        connection
            .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT body FROM messages WHERE account = ?1 AND uid = ?2",
                [account, uid],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        body
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM otp_notifications WHERE account = ?1 AND uid = ?2",
                [account, uid],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}

fn no_restore_tempfiles(directory: &Path) -> bool {
    fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".maild-restore-")
    })
}

#[test]
fn file_store_lock_excludes_second_owner_and_releases_on_drop() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let first = Store::open(database.to_str().unwrap()).unwrap();

    let error = match Store::open(database.to_str().unwrap()) {
        Ok(_) => panic!("同一数据库只能有一个 Store owner"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        StoreOpenError::Lock(DatabaseLockError::InUse { .. })
    ));
    drop(first);

    Store::open(database.to_str().unwrap()).expect("owner 释放后可重新打开");
    let lock_files = fs::read_dir(directory.path())
        .unwrap()
        .filter_map(|entry| {
            let name = entry.ok()?.file_name();
            name.to_string_lossy()
                .ends_with(".maild.lock")
                .then_some(name)
        })
        .count();
    assert_eq!(lock_files, 1, "锁 inode 必须持久复用，不能在 drop 时删除");
}

#[cfg(unix)]
#[test]
fn unix_lock_rejects_writable_directories_unsafe_files_and_database_symlinks() {
    use std::os::unix::fs::{symlink, PermissionsExt as _};

    let root = tempfile::tempdir().unwrap();
    let writable = root.path().join("writable");
    fs::create_dir(&writable).unwrap();
    fs::set_permissions(&writable, fs::Permissions::from_mode(0o770)).unwrap();
    let database = writable.join(maild_brand::DATABASE_FILE_NAME);
    assert!(matches!(
        Store::open(database.to_str().unwrap()),
        Err(StoreOpenError::Lock(
            DatabaseLockError::UnsafeDatabaseDirectory(_)
        ))
    ));
    assert!(!database.exists());

    let safe = root.path().join("safe");
    fs::create_dir(&safe).unwrap();
    fs::set_permissions(&safe, fs::Permissions::from_mode(0o700)).unwrap();
    let unsafe_lock = safe.join(format!(".{}.maild.lock", maild_brand::DATABASE_FILE_NAME));
    fs::write(&unsafe_lock, b"").unwrap();
    fs::set_permissions(&unsafe_lock, fs::Permissions::from_mode(0o644)).unwrap();
    let database = safe.join(maild_brand::DATABASE_FILE_NAME);
    assert!(matches!(
        Store::open(database.to_str().unwrap()),
        Err(StoreOpenError::Lock(DatabaseLockError::UnsafeLockFile(_)))
    ));

    fs::set_permissions(&unsafe_lock, fs::Permissions::from_mode(0o600)).unwrap();
    let real = safe.join("real.sqlite3");
    create_store(&real, "real", "1", "real");
    let linked = safe.join("linked.sqlite3");
    symlink(&real, &linked).unwrap();
    assert!(matches!(
        Store::open(linked.to_str().unwrap()),
        Err(StoreOpenError::Lock(
            DatabaseLockError::InvalidDatabasePath(_)
        ))
    ));
}

#[cfg(any(unix, windows))]
#[test]
fn database_hardlink_alias_cannot_acquire_a_second_lock_identity() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let running = Store::open(database.to_str().unwrap()).unwrap();
    let hardlink = directory.path().join("maild-alias.sqlite3");
    fs::hard_link(&database, &hardlink).unwrap();

    assert!(matches!(
        Store::open(hardlink.to_str().unwrap()),
        Err(StoreOpenError::Lock(
            DatabaseLockError::InvalidDatabasePath(_)
        ))
    ));
    let input = directory.path().join("input.sqlite3");
    create_store(&input, "new", "2", "replacement");
    let rollback = directory.path().join("rollback.sqlite3");
    assert!(matches!(
        restore_database(&hardlink, &input, &rollback),
        Err(RestoreError::Lock(DatabaseLockError::InvalidDatabasePath(
            _
        ))) | Err(RestoreError::InvalidDatabase(_))
    ));
    assert!(!rollback.exists());
    assert_eq!(running.count().unwrap(), 0);
}

#[cfg(unix)]
#[test]
fn unix_store_tightens_existing_database_permissions_and_creates_private_sidecars() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    create_store(&database, "existing", "0", "existing");
    let raw = Connection::open(&database).unwrap();
    raw.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    raw.execute(
        "INSERT INTO messages (uid, account, sender, subject, body, received_at)
         VALUES ('old-wal', 'private', 's', 's', 'not checkpointed', 1)",
        [],
    )
    .unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o644)).unwrap();
    for suffix in ["-wal", "-shm"] {
        let sidecar = sqlite_sidecar(&database, suffix);
        assert!(sidecar.exists(), "测试必须保留现有 {suffix}");
        fs::set_permissions(sidecar, fs::Permissions::from_mode(0o644)).unwrap();
    }
    let journal = sqlite_sidecar(&database, "-journal");
    fs::write(&journal, b"").unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o644)).unwrap();

    let store =
        Store::open(database.to_str().unwrap()).expect("已有 owner 的库和 sidecar 自动收紧权限");
    store
        .insert_message(&message("private", "1", "secret"))
        .unwrap();
    assert_eq!(
        fs::metadata(&database).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = sqlite_sidecar(&database, suffix);
        if let Ok(metadata) = fs::metadata(sidecar) {
            assert_eq!(metadata.permissions().mode() & 0o077, 0);
        }
    }
    drop(raw);
}

#[cfg(unix)]
#[test]
fn unix_special_sidecar_fails_fast_without_blocking_daemon_startup() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let config = directory.path().join(maild_brand::CONFIG_FILE_NAME);
    create_store(&database, "existing", "0", "existing");
    let wal = sqlite_sidecar(&database, "-wal");
    let wal_bytes = CString::new(wal.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(wal_bytes.as_ptr(), 0o600) }, 0);
    fs::write(
        &config,
        format!(
            "[plugins.store]\n[plugins.store.config]\ndb_path = {:?}\n",
            database.to_str().unwrap()
        ),
    )
    .unwrap();

    let mut child = Command::new(maild_brand::bin_exe_daemon!())
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("特殊 sidecar 必须快速 fail-closed，不能阻塞 open");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    assert_eq!(status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("store 插件未能"));
}

#[test]
fn restore_is_refused_while_store_is_running_in_another_process() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let input = directory.path().join("input.sqlite3");
    let rollback = directory.path().join("rollback.sqlite3");
    let running = Store::open(database.to_str().unwrap()).unwrap();
    running
        .insert_message(&message("old", "1", "running"))
        .unwrap();
    create_store(&input, "new", "2", "replacement");

    let output = Command::new(maild_brand::bin_exe_daemon!())
        .args(["restore", "--database"])
        .arg(&database)
        .arg("--input")
        .arg(&input)
        .arg("--rollback")
        .arg(&rollback)
        .output()
        .expect("运行受锁保护的 restore");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("正在被另一个"));
    assert!(!rollback.exists());
    assert_eq!(running.count().unwrap(), 1);
}

#[test]
fn daemon_exits_instead_of_running_without_a_configured_locked_store() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let config = directory.path().join(maild_brand::CONFIG_FILE_NAME);
    let running = Store::open(database.to_str().unwrap()).unwrap();
    fs::write(
        &config,
        format!(
            "[plugins.store]\n[plugins.store.config]\ndb_path = {:?}\n",
            database.to_str().unwrap()
        ),
    )
    .unwrap();

    let mut child = Command::new(maild_brand::bin_exe_daemon!())
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动第二个 daemon");
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("第二个 daemon 不得在 store 装配失败后继续运行");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    assert_eq!(status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("store 插件未能取得数据库所有权"));
    assert!(stderr.lines().count() <= 2);
    assert_eq!(running.count().unwrap(), 0);
}

#[test]
fn restore_atomically_installs_verified_input_and_preserves_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let input = directory.path().join("input.sqlite3");
    let rollback = directory.path().join("rollback.sqlite3");
    create_store(&database, "old", "1", "before restore");
    create_store(&input, "new", "2", "after restore");
    let input_bytes = fs::read(&input).unwrap();

    let outcome = restore_database(&database, &input, &rollback).expect("restore 成功");
    assert_eq!(outcome.database, fs::canonicalize(&database).unwrap());
    assert_eq!(
        outcome.rollback,
        fs::canonicalize(directory.path())
            .unwrap()
            .join("rollback.sqlite3")
    );
    assert!(!sqlite_sidecar(&database, "-wal").exists());
    assert!(!sqlite_sidecar(&database, "-shm").exists());
    assert!(no_restore_tempfiles(directory.path()));
    assert_database(&database, "new", "2", "after restore");
    assert_database(&rollback, "old", "1", "before restore");
    assert_eq!(fs::read(&input).unwrap(), input_bytes, "恢复输入保持不变");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&database).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&rollback).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let reopened = Store::open(database.to_str().unwrap()).expect("恢复后 daemon 可重新打开");
    assert_eq!(reopened.latest().unwrap().unwrap().body, "after restore");
}

#[test]
fn invalid_input_and_existing_rollback_leave_current_database_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let input = directory.path().join("corrupt.sqlite3");
    let rollback = directory.path().join("rollback.sqlite3");
    create_store(&database, "old", "1", "keep me");
    fs::write(&input, b"not sqlite").unwrap();

    assert!(restore_database(&database, &input, &rollback).is_err());
    assert!(!rollback.exists());
    assert_database(&database, "old", "1", "keep me");
    assert!(no_restore_tempfiles(directory.path()));

    fs::remove_file(&input).unwrap();
    {
        let malformed = Connection::open(&input).unwrap();
        malformed
            .execute_batch(
                "CREATE TABLE wrong_shape (value TEXT NOT NULL);
                 PRAGMA user_version=1;",
            )
            .unwrap();
    }
    let error = restore_database(&database, &input, &rollback).unwrap_err();
    assert!(matches!(error, RestoreError::Schema(_) | RestoreError::Backup(_)));
    assert!(!rollback.exists(), "畸形 v1 输入必须在创建 rollback 前拒绝");
    assert_database(&database, "old", "1", "keep me");
    assert!(no_restore_tempfiles(directory.path()));

    fs::remove_file(&input).unwrap();
    create_store(&input, "new", "2", "replacement");
    fs::write(&rollback, b"do not overwrite").unwrap();
    let original_rollback = fs::read(&rollback).unwrap();
    let error = restore_database(&database, &input, &rollback).unwrap_err();
    assert!(matches!(error, RestoreError::InvalidRollback(_)));
    assert_eq!(fs::read(&rollback).unwrap(), original_rollback);
    assert_database(&database, "old", "1", "keep me");
}

/// ADR-0022：v1 备份可恢复进新库——发布后迁移到当前 schema，数据保留。
#[test]
fn v1_backup_input_is_restored_and_migrated_to_current_schema() {
    const V1_SCHEMA: &str = "
CREATE TABLE messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  uid TEXT NOT NULL,
  account TEXT NOT NULL,
  sender TEXT NOT NULL,
  subject TEXT NOT NULL,
  body TEXT NOT NULL,
  received_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_messages_account_uid ON messages(account, uid);
CREATE INDEX idx_messages_account_id ON messages(account, id DESC);
CREATE TABLE otp_notifications (
  account TEXT NOT NULL,
  uid TEXT NOT NULL,
  notified_at INTEGER NOT NULL,
  PRIMARY KEY (account, uid)
);
PRAGMA user_version=1;
";
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let input = directory.path().join("v1-backup.sqlite3");
    let rollback = directory.path().join("rollback.sqlite3");
    create_store(&database, "old", "1", "current db");

    {
        let conn = Connection::open(&input).unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO messages
             (uid, account, sender, subject, body, received_at)
             VALUES ('9', 'restored', 's@t', 'm', 'from v1 backup', 7)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO otp_notifications (account, uid, notified_at)
             VALUES ('restored', '9', 7)",
            [],
        )
        .unwrap();
        drop(conn);
    }

    restore_database(&database, &input, &rollback).expect("v1 备份应恢复成功");
    // 发布产物已迁移到当前版本，消息与 OTP 标记完整保留。
    assert_database(&database, "restored", "9", "from v1 backup");
    {
        let conn =
            Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT recipient FROM messages WHERE account = 'restored' AND uid = '9'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "",
            "v1 行的收件人回填空串"
        );
    }
    assert_database(&rollback, "old", "1", "current db");
}

/// ADR-0025：v2 备份同样可恢复进新库——发布后迁移到 v3，数据保留。
/// v3 之后 v2 成为历史版本，因此它必须有自己固化的结构快照校验。
#[test]
fn v2_backup_input_is_restored_and_migrated_to_current_schema() {
    const V2_SCHEMA: &str = "
CREATE TABLE messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  uid TEXT NOT NULL,
  account TEXT NOT NULL,
  sender TEXT NOT NULL,
  subject TEXT NOT NULL,
  body TEXT NOT NULL,
  received_at INTEGER NOT NULL,
  recipient TEXT NOT NULL DEFAULT ''
);
CREATE UNIQUE INDEX idx_messages_account_uid ON messages(account, uid);
CREATE INDEX idx_messages_account_id ON messages(account, id DESC);
CREATE TABLE otp_notifications (
  account TEXT NOT NULL,
  uid TEXT NOT NULL,
  notified_at INTEGER NOT NULL,
  PRIMARY KEY (account, uid)
);
CREATE TABLE addresses (
  id TEXT PRIMARY KEY NOT NULL,
  account TEXT NOT NULL,
  address TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('alias', 'disposable')),
  label TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL CHECK(state IN ('active', 'disabled', 'expired')),
  created_at INTEGER NOT NULL CHECK(created_at >= 0),
  expires_at INTEGER CHECK(expires_at IS NULL OR expires_at >= 0)
);
CREATE UNIQUE INDEX idx_addresses_address ON addresses(address);
CREATE INDEX idx_addresses_account ON addresses(account, id);
PRAGMA user_version=2;
";
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let input = directory.path().join("v2-backup.sqlite3");
    let rollback = directory.path().join("rollback.sqlite3");
    create_store(&database, "old", "1", "current db");

    {
        let conn = Connection::open(&input).unwrap();
        conn.execute_batch(V2_SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO messages
             (uid, account, sender, subject, body, received_at, recipient)
             VALUES ('9', 'restored', 's@t', 'm', 'from v2 backup', 7, 'alias@example.test')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO otp_notifications (account, uid, notified_at)
             VALUES ('restored', '9', 7)",
            [],
        )
        .unwrap();
    }

    restore_database(&database, &input, &rollback).expect("v2 备份应恢复成功");
    assert_database(&database, "restored", "9", "from v2 backup");
    {
        let conn =
            Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let (recipient, sort_at, date_at) = conn
            .query_row(
                "SELECT recipient, sort_at, date_at FROM messages
                 WHERE account = 'restored' AND uid = '9'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(recipient, "alias@example.test", "v2 的收件人保留");
        assert_eq!(sort_at, 7, "排序键回填为投递观测时间");
        assert_eq!(date_at, None);
    }
    assert_database(&rollback, "old", "1", "current db");
}

#[test]
fn future_schema_input_is_rejected_without_creating_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let input = directory.path().join("future.sqlite3");
    let rollback = directory.path().join("rollback.sqlite3");
    create_store(&database, "old", "1", "keep me");
    create_store(&input, "new", "2", "future");
    let connection = Connection::open(&input).unwrap();
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(connection);

    assert!(matches!(
        restore_database(&database, &input, &rollback),
        Err(RestoreError::Schema(
            StoreOpenError::UnsupportedSchemaVersion { .. }
        ))
    ));
    assert!(!rollback.exists());
    assert_database(&database, "old", "1", "keep me");
}

#[test]
fn restore_rejects_current_sqlite_sidecars_as_input_or_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let input = directory.path().join("input.sqlite3");
    let rollback_wal = sqlite_sidecar(&database, "-wal");
    create_store(&database, "old", "1", "keep me");
    create_store(&input, "new", "2", "replacement");

    assert!(matches!(
        restore_database(&database, &input, &rollback_wal),
        Err(RestoreError::ConflictingPaths)
    ));
    assert!(!rollback_wal.exists());
    assert_database(&database, "old", "1", "keep me");

    let input_shm = sqlite_sidecar(&database, "-shm");
    fs::write(&input_shm, b"looks like an input path").unwrap();
    let rollback = directory.path().join("rollback.sqlite3");
    assert!(matches!(
        restore_database(&database, &input_shm, &rollback),
        Err(RestoreError::ConflictingPaths)
    ));
    assert!(!rollback.exists());
    fs::remove_file(input_shm).unwrap();
    assert_database(&database, "old", "1", "keep me");

    let unicode_database = directory.path().join("Ä.sqlite3");
    create_store(&unicode_database, "unicode", "1", "keep unicode");
    let unicode_alias = directory.path().join("ä.sqlite3-WAL");
    assert!(matches!(
        restore_database(&unicode_database, &input, &unicode_alias),
        Err(RestoreError::ConflictingPaths)
    ));
    assert!(!unicode_alias.exists());
    assert_database(&unicode_database, "unicode", "1", "keep unicode");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let sidecar_symlink = sqlite_sidecar(&database, "-journal");
        symlink(&input, &sidecar_symlink).unwrap();
        assert!(matches!(
            restore_database(&database, &sidecar_symlink, &rollback),
            Err(RestoreError::InvalidInput(_))
        ));
        fs::remove_file(sidecar_symlink).unwrap();
        assert!(!rollback.exists());
        assert_database(&database, "old", "1", "keep me");
    }
}

#[cfg(unix)]
#[test]
fn post_rollback_publish_failure_keeps_original_and_verified_rollback() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let input = directory.path().join("input.sqlite3");
    let rollback = directory.path().join("rollback.sqlite3");
    create_store(&database, "old", "1", "keep me");
    create_store(&input, "new", "2", "replacement");
    fs::set_permissions(&database, fs::Permissions::from_mode(0o400)).unwrap();

    let error = restore_database(&database, &input, &rollback).unwrap_err();
    assert!(matches!(error, RestoreError::AfterRollback { .. }));
    assert_database(&database, "old", "1", "keep me");
    assert_database(&rollback, "old", "1", "keep me");
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn maild_restore_cli_success_help_and_argument_contract() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let input = directory.path().join("input.sqlite3");
    let rollback = directory.path().join("rollback.sqlite3");
    create_store(&database, "old", "1", "before cli");
    create_store(&input, "new", "2", "after cli");

    let output = Command::new(maild_brand::bin_exe_daemon!())
        .args(["restore", "--database"])
        .arg(&database)
        .arg("--input")
        .arg(&input)
        .arg("--rollback")
        .arg(&rollback)
        .output()
        .expect("运行 maild restore");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("恢复完成"));
    assert!(stdout.contains("回滚快照"));
    assert_database(&database, "new", "2", "after cli");
    assert_database(&rollback, "old", "1", "before cli");

    let help = Command::new(maild_brand::bin_exe_daemon!())
        .args(["restore", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains(&format!("Usage: {} restore", maild_brand::CLI_DAEMON)));
    assert!(help.contains("--database"));
    assert!(help.contains("--input"));
    assert!(help.contains("--rollback"));

    let bad_args = Command::new(maild_brand::bin_exe_daemon!())
        .args(["restore", "--database"])
        .arg(&database)
        .output()
        .unwrap();
    assert_eq!(bad_args.status.code(), Some(2));
    assert!(bad_args.stdout.is_empty());
}
