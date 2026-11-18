use std::fs::{self, OpenOptions};
use std::io::{Seek as _, Write as _};
use std::path::Path;
use std::process::Command;

use maild::events::MailMessage;
use maild::store::{backup_database, BackupError, Store, SCHEMA_VERSION};
use rusqlite::{Connection, OpenFlags};

fn message(uid: &str, body: String) -> MailMessage {
    MailMessage {
        account: "backup".into(),
        uid: uid.into(),
        sender: "sender@example.test".into(),
        recipient: String::new(),
        subject: "online snapshot".into(),
        body,
        received_at: 42,
        ..Default::default()
    }
}

fn assert_backup_contents(path: &Path, expected_body: &str) {
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
                "SELECT body FROM messages WHERE account = 'backup' AND uid = '7'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        expected_body
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM otp_notifications
                 WHERE account = 'backup' AND uid = '7'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn online_backup_captures_live_wal_schema_messages_and_otp_markers() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let destination = directory.path().join("maild-backup.sqlite3");
    let store = Store::open(source.to_str().unwrap()).unwrap();
    let body = "长正文".repeat(100_000);
    assert!(store.insert_message(&message("7", body.clone())).unwrap());
    assert!(store.mark_notified("backup", "7").unwrap());

    let mut wal_path = source.as_os_str().to_os_string();
    wal_path.push("-wal");
    assert!(
        fs::metadata(wal_path).is_ok_and(|metadata| metadata.len() > 0),
        "测试必须从仍有 WAL sidecar 的打开数据库取快照"
    );

    backup_database(&source, &destination).expect("在线备份成功");
    assert_backup_contents(&destination, &body);
    assert_eq!(store.count().unwrap(), 1, "备份不能影响仍打开的源 store");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn existing_destination_is_rejected_without_overwrite() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let destination = directory.path().join("existing.sqlite3");
    drop(Store::open(source.to_str().unwrap()).unwrap());
    fs::write(&destination, b"keep this file").unwrap();

    let error = backup_database(&source, &destination).unwrap_err();
    assert!(matches!(error, BackupError::DestinationExists(path) if path == destination));
    assert_eq!(fs::read(&destination).unwrap(), b"keep this file");
}

#[test]
fn invalid_source_leaves_no_destination_or_temporary_file() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("corrupt.sqlite3");
    let destination = directory.path().join("backup.sqlite3");
    fs::write(&source, b"this is not sqlite").unwrap();

    assert!(backup_database(&source, &destination).is_err());
    assert!(!destination.exists());
    let names = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert!(
        names
            .iter()
            .all(|name| !name.to_string_lossy().starts_with(".maild-backup-")),
        "失败不能遗留临时备份: {names:?}"
    );

    let versioned_source = directory.path().join("versioned.sqlite3");
    drop(Store::open(versioned_source.to_str().unwrap()).unwrap());
    let connection = Connection::open(&versioned_source).unwrap();
    connection.pragma_update(None, "user_version", 0).unwrap();
    drop(connection);
    let legacy_destination = directory.path().join("legacy-backup.sqlite3");
    assert!(matches!(
        backup_database(&versioned_source, &legacy_destination),
        Err(BackupError::Schema(_))
    ));
    assert!(!legacy_destination.exists());
    let connection = Connection::open(&versioned_source).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        0,
        "备份不得迁移旧 schema"
    );
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .unwrap();
    drop(connection);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let unsafe_directory = directory.path().join("group-writable");
        fs::create_dir(&unsafe_directory).unwrap();
        fs::set_permissions(&unsafe_directory, fs::Permissions::from_mode(0o770)).unwrap();
        let unsafe_destination = unsafe_directory.join("backup.sqlite3");
        let error = backup_database(&versioned_source, &unsafe_destination).unwrap_err();
        assert!(matches!(error, BackupError::UnsafeDestinationDirectory(_)));
        assert!(!unsafe_destination.exists());
    }
}

#[test]
fn corrupt_extra_index_fails_after_copy_without_leaking_diagnostics_or_tempfiles() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let destination = directory.path().join("backup.sqlite3");
    let store = Store::open(source.to_str().unwrap()).unwrap();
    store.insert_message(&message("7", "body".into())).unwrap();
    drop(store);

    let malicious_index = "evil\n\u{1b}[31m_index";
    let connection = Connection::open(&source).unwrap();
    connection
        .execute(
            &format!("CREATE INDEX \"{malicious_index}\" ON messages(subject)"),
            [],
        )
        .unwrap();
    let root_page = connection
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = ?1",
            [malicious_index],
            |row| row.get::<_, u64>(0),
        )
        .unwrap();
    let page_size = connection
        .pragma_query_value(None, "page_size", |row| row.get::<_, u64>(0))
        .unwrap();
    drop(connection);

    let mut source_file = OpenOptions::new().write(true).open(&source).unwrap();
    source_file
        .seek(std::io::SeekFrom::Start((root_page - 1) * page_size))
        .unwrap();
    source_file.write_all(&[0; 16]).unwrap();
    source_file.sync_all().unwrap();

    let error = backup_database(&source, &destination).unwrap_err();
    assert!(matches!(error, BackupError::IntegrityCheck(_)));
    let rendered = error.to_string();
    assert!(!rendered.contains('\n'));
    assert!(!rendered.contains('\u{1b}'));
    assert!(!destination.exists());

    let cli_destination = directory.path().join("cli-backup.sqlite3");
    let output = Command::new(maild_brand::bin_exe_daemon!())
        .args(["backup", "--database"])
        .arg(&source)
        .arg("--output")
        .arg(&cli_destination)
        .output()
        .expect("运行损坏源备份");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim_end_matches(&['\r', '\n'][..]);
    assert!(!stderr.chars().any(|character| character.is_control()));
    assert!(stderr.contains("完整性校验失败"));
    assert!(!cli_destination.exists());

    let names = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert!(
        names
            .iter()
            .all(|name| !name.to_string_lossy().starts_with(".maild-backup-")),
        "post-temp 失败不能遗留临时备份: {names:?}"
    );
}

#[test]
fn maild_backup_cli_creates_verified_snapshot_and_refuses_second_publish() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join(maild_brand::DATABASE_FILE_NAME);
    let destination = directory.path().join("cli-backup.sqlite3");
    let store = Store::open(source.to_str().unwrap()).unwrap();
    let body = "cli snapshot".to_owned();
    store.insert_message(&message("7", body.clone())).unwrap();
    store.mark_notified("backup", "7").unwrap();

    let output = Command::new(maild_brand::bin_exe_daemon!())
        .args(["backup", "--database"])
        .arg(&source)
        .arg("--output")
        .arg(&destination)
        .output()
        .expect("运行 maild backup");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("备份完成"));
    assert!(output.stderr.is_empty());
    assert_backup_contents(&destination, &body);

    let original = fs::read(&destination).unwrap();
    let second = Command::new(maild_brand::bin_exe_daemon!())
        .args(["backup", "--database"])
        .arg(&source)
        .arg("--output")
        .arg(&destination)
        .output()
        .expect("再次运行 maild backup");
    assert_eq!(second.status.code(), Some(1));
    assert!(second.stdout.is_empty());
    assert!(String::from_utf8_lossy(&second.stderr).contains("不会覆盖"));
    assert_eq!(fs::read(&destination).unwrap(), original);
}

#[test]
fn maild_backup_cli_help_and_argument_errors_are_stable() {
    let help = Command::new(maild_brand::bin_exe_daemon!())
        .args(["backup", "--help"])
        .output()
        .expect("运行 maild backup --help");
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(help_text.contains(&format!("Usage: {} backup", maild_brand::CLI_DAEMON)));
    assert!(help_text.contains("--database"));
    assert!(help_text.contains("--output"));

    let output = Command::new(maild_brand::bin_exe_daemon!())
        .args(["backup", "--database", "missing.sqlite3"])
        .output()
        .expect("运行参数不完整的 maild backup");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--output"));
}

#[test]
fn backup_path_errors_escape_terminal_control_characters() {
    let rendered = BackupError::DestinationExists("bad\n\u{1b}[31m.sqlite3".into()).to_string();
    assert!(!rendered.contains('\n'));
    assert!(!rendered.contains('\u{1b}'));
    assert!(rendered.contains("\\n"));

    let rendered =
        BackupError::IntegrityCheck(vec!["bad\n\u{1b}[31m index".into(), "x".repeat(10_000)])
            .to_string();
    assert!(!rendered.chars().any(|character| character.is_control()));
    assert!(rendered.chars().count() <= 2_100);
    assert!(rendered.ends_with('…'));
}
