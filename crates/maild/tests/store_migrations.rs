use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use maild::events::MailMessage;
use maild::store::{Store, StoreOpenError, SCHEMA_VERSION};
use rusqlite::{params, Connection};

static NEXT_DB_ID: AtomicU64 = AtomicU64::new(1);

const LEGACY_SCHEMA: &str = "
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
CREATE TABLE otp_notifications (
  account TEXT NOT NULL,
  uid TEXT NOT NULL,
  notified_at INTEGER NOT NULL,
  PRIMARY KEY (account, uid)
);
";

struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let id = NEXT_DB_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            path: std::env::temp_dir().join(format!(
                "maild-store-{label}-{}-{id}.sqlite3",
                std::process::id()
            )),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn path_str(&self) -> &str {
        self.path.to_str().expect("临时数据库路径必须是 UTF-8")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

fn schema_version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn message(uid: &str) -> MailMessage {
    MailMessage {
        account: "legacy".into(),
        uid: uid.into(),
        sender: "sender@example.test".into(),
        recipient: String::new(),
        subject: "migration".into(),
        body: "preserved".into(),
        received_at: 42,
        // ADR-0025：v3 迁移把旧行的 sort_at 回填为 received_at，
        // 因此升级不会重排既有列表。
        sort_at: 42,
        // ADR-0026：v4 迁移把历史行挂到自动创建的 INBOX 上。
        folder: "INBOX".into(),
        folder_role: "inbox".into(),
        // ADR-0029：v7 迁移**不**索引历史行，thread_id 保持 0 等待后台回填。
        thread_id: 0,
        ..Default::default()
    }
}

#[test]
fn new_database_is_created_at_current_schema_version() {
    let db = TempDb::new("new");
    let store = Store::open(db.path_str()).expect("新数据库应创建成功");
    assert_eq!(store.count().unwrap(), 0);
    drop(store);

    assert_eq!(schema_version(db.path()), SCHEMA_VERSION);
}

#[test]
fn legacy_v0_database_migrates_without_losing_data() {
    let db = TempDb::new("legacy");
    let conn = Connection::open(db.path()).unwrap();
    conn.execute_batch(LEGACY_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO messages
         (uid, account, sender, subject, body, received_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "7",
            "legacy",
            "sender@example.test",
            "migration",
            "preserved",
            42
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO otp_notifications (account, uid, notified_at)
         VALUES ('legacy', '7', 42)",
        [],
    )
    .unwrap();
    drop(conn);

    let store = Store::open(db.path_str()).expect("历史数据库应迁移成功");
    assert_eq!(store.count().unwrap(), 1);
    assert_eq!(store.latest().unwrap(), Some(message("7")));
    assert!(store.is_notified("legacy", "7").unwrap());
    drop(store);

    assert_eq!(schema_version(db.path()), SCHEMA_VERSION);
}

/// v1 形状：与 v0 相比多了 id 倒序索引；无 recipient 列（ADR-0022 前）。
const SCHEMA_V1_EXACT: &str = "
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

#[test]
fn v1_database_migrates_to_current_and_backfills_recipient() {
    let db = TempDb::new("v1-recipient");
    {
        let conn = Connection::open(db.path()).unwrap();
        conn.execute_batch(SCHEMA_V1_EXACT).unwrap();
        for uid in ["101", "102"] {
            conn.execute(
                "INSERT INTO messages
                 (uid, account, sender, subject, body, received_at)
                 VALUES (?1, 'legacy', 's@t', 'm', 'b', 7)",
                params![uid],
            )
            .unwrap();
        }
        // v1 库里预置一张 addresses 表应属异常形状——迁移只管自己的表，
        // 这里不制造该场景（fail-closed 校验在打开后由 validate_schema 兜底）。
    }

    let store = Store::open(db.path_str()).expect("v1 数据库应迁移到当前版本");
    assert_eq!(schema_version(db.path()), SCHEMA_VERSION);
    assert_eq!(store.count().unwrap(), 2);
    let latest = store.latest().unwrap().expect("消息保留");
    assert_eq!(latest.recipient, "", "旧行的收件人回填空串");
    assert_eq!(latest.uid, "102");

    // 迁移后的库可继续正常写入带收件人的新消息。
    let mut with_recipient = message("103");
    with_recipient.recipient = "shop@example.test".into();
    assert!(store.insert_message(&with_recipient).unwrap());
    assert_eq!(
        store.get_message("legacy", None, "103").unwrap().unwrap().recipient,
        "shop@example.test"
    );
}

/// v2 形状：v1 + recipient 列 + 地址层表（ADR-0022），无 v3 对象。
const SCHEMA_V2_EXACT: &str = "
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

/// ADR-0025：v2 库升 v3，消息 / 地址 / OTP 标记全部保留，
/// 且 `sort_at` 回填为 `received_at`——升级不重排既有列表。
#[test]
fn v2_database_migrates_to_current_and_backfills_sort_key() {
    let db = TempDb::new("v2");
    {
        let conn = Connection::open(db.path()).unwrap();
        conn.execute_batch(SCHEMA_V2_EXACT).unwrap();
        conn.execute(
            "INSERT INTO messages
             (uid, account, sender, subject, body, received_at, recipient)
             VALUES ('7', 'legacy', 'sender@example.test', 'migration', 'preserved', 42,
                     'alias@example.test')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO otp_notifications (account, uid, notified_at)
             VALUES ('legacy', '7', 42)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO addresses (id, account, address, kind, label, state, created_at)
             VALUES ('addr', 'legacy', 'alias@example.test', 'alias', '', 'active', 1)",
            [],
        )
        .unwrap();
    }

    let store = Store::open(db.path_str()).expect("v2 数据库应迁移成功");
    let migrated = store.get_message("legacy", None, "7").unwrap().expect("消息保留");
    assert_eq!(migrated.body, "preserved");
    assert_eq!(migrated.recipient, "alias@example.test");
    assert_eq!(migrated.received_at, 42);
    assert_eq!(migrated.sort_at, 42, "旧行的排序键回填为投递观测时间");
    assert_eq!(migrated.date_at, None, "历史行没有 Date 头可回填");
    assert_eq!(migrated.body_html, "");
    assert!(!migrated.has_attachments);
    assert!(store.is_notified("legacy", "7").unwrap());
    assert_eq!(store.count_addresses("legacy").unwrap(), 1, "地址层保留");
    drop(store);

    assert_eq!(schema_version(db.path()), SCHEMA_VERSION);
}

/// v2 库里若已存在 v3 的保留对象，`CREATE IF NOT EXISTS` 会绕过约束定义，
/// 因此迁移必须 fail-closed 而不是静默沿用它。
#[test]
fn v2_database_carrying_reserved_v3_object_is_rejected() {
    let db = TempDb::new("v2-reserved");
    {
        let conn = Connection::open(db.path()).unwrap();
        conn.execute_batch(SCHEMA_V2_EXACT).unwrap();
        conn.execute_batch("CREATE TABLE attachments (whatever TEXT);")
            .unwrap();
    }

    let error = match Store::open(db.path_str()) {
        Ok(_) => panic!("含保留对象的 v2 库必须拒绝"),
        Err(error) => error,
    };
    assert!(matches!(error, StoreOpenError::InvalidSchema(_)));
    assert_eq!(schema_version(db.path()), 2, "失败迁移不能提升版本");

    let conn = Connection::open(db.path()).unwrap();
    let created: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN
               ('folders', 'idx_folders_account_name', 'idx_messages_sort',
                'idx_messages_account_sort')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(created, 0, "失败迁移创建的 v3 对象必须一起回滚");
    let columns: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'sort_at'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 0, "失败迁移新增的列必须一起回滚");
}

/// v8 = v7 + 两张草稿表（ADR-0034 决策 2）。把它们摘掉再把版本号写回 7，
/// 得到的就是一个真的 v7 库；重开必须补上表而不动既有数据。
fn downgrade_to_v7(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "DROP INDEX idx_draft_attachments_slot;
         DROP TABLE draft_attachments;
         DROP INDEX idx_drafts_account_updated;
         DROP TABLE drafts;",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 7i64).unwrap();
}

#[test]
fn v7_database_gains_the_draft_tables_without_touching_mail() {
    let db = TempDb::new("v7-drafts");
    {
        let store = Store::open(db.path_str()).unwrap();
        store.insert_message(&message("21")).unwrap();
    }
    downgrade_to_v7(db.path());
    assert_eq!(schema_version(db.path()), 7);

    let store = Store::open(db.path_str()).expect("v7 库应升到当前版本");
    assert_eq!(store.count().unwrap(), 1, "邮件一封不少");
    assert_eq!(store.list_drafts(None, 10, 0).unwrap().1, 0, "新表是空的");
    let id = maild::drafts::generate_draft_id();
    store
        .create_draft(
            &id,
            "legacy",
            &maild::drafts::DraftPatch {
                body: Some("升级后写的".into()),
                ..Default::default()
            },
            99,
        )
        .unwrap();
    drop(store);

    assert_eq!(schema_version(db.path()), SCHEMA_VERSION);
    let reopened = Store::open(db.path_str()).unwrap();
    assert_eq!(reopened.get_draft(&id).unwrap().unwrap().body, "升级后写的");
}

#[test]
fn v7_database_carrying_a_reserved_draft_object_is_rejected() {
    let db = TempDb::new("v7-reserved");
    {
        let store = Store::open(db.path_str()).unwrap();
        store.insert_message(&message("22")).unwrap();
    }
    downgrade_to_v7(db.path());
    {
        // 形状不对的同名表：`CREATE TABLE IF NOT EXISTS` 会静默接受它，
        // 于是草稿会写进一张没有约束的表里。保留名必须在迁移前就拒绝。
        let conn = Connection::open(db.path()).unwrap();
        conn.execute_batch("CREATE TABLE drafts (whatever TEXT);")
            .unwrap();
    }

    let error = match Store::open(db.path_str()) {
        Ok(_) => panic!("含保留对象的 v7 库必须拒绝"),
        Err(error) => error,
    };
    assert!(matches!(error, StoreOpenError::InvalidSchema(_)));
    assert_eq!(schema_version(db.path()), 7, "失败迁移不能提升版本");
    let conn = Connection::open(db.path()).unwrap();
    let created: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'draft_attachments'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(created, 0, "失败迁移创建的 v8 对象必须一起回滚");
}

#[test]
fn current_schema_reopens_idempotently() {
    let db = TempDb::new("current");
    let store = Store::open(db.path_str()).unwrap();
    store.insert_message(&message("8")).unwrap();
    drop(store);

    let reopened = Store::open(db.path_str()).expect("当前 schema 应可重复打开");
    assert_eq!(reopened.count().unwrap(), 1);
    let stored = reopened.latest().unwrap().expect("消息仍在");
    // 经落库路径写入的消息当场进线程图，不用等回填（与上面迁移来的历史行相反）。
    assert!(stored.thread_id > 0, "落库路径直接分配线程 id");
    assert_eq!(
        stored,
        MailMessage {
            thread_id: stored.thread_id,
            ..message("8")
        }
    );
}

#[test]
fn future_schema_is_rejected_without_mutation() {
    let db = TempDb::new("future");
    let conn = Connection::open(db.path()).unwrap();
    conn.execute_batch(
        &format!(
            "PRAGMA user_version={};
             CREATE TABLE future_data (value TEXT NOT NULL);
             INSERT INTO future_data VALUES ('keep');",
            SCHEMA_VERSION + 1
        ),
    )
    .unwrap();
    let journal_mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    drop(conn);

    let error = match Store::open(db.path_str()) {
        Ok(_) => panic!("未来 schema 必须 fail-closed"),
        Err(error) => error,
    };
    assert!(
        matches!(error, StoreOpenError::UnsupportedSchemaVersion { supported, .. }
            if supported == SCHEMA_VERSION),
        "实际错误: {error:?}"
    );

    let conn = Connection::open(db.path()).unwrap();
    assert_eq!(schema_version(db.path()), SCHEMA_VERSION + 1);
    assert_eq!(
        conn.query_row("SELECT value FROM future_data", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "keep"
    );
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        journal_mode,
        "拒绝未来 schema 前不能切换 journal mode"
    );
}

#[test]
fn malformed_legacy_schema_rolls_back_migration() {
    let db = TempDb::new("malformed");
    let conn = Connection::open(db.path()).unwrap();
    conn.execute_batch(
        "CREATE TABLE messages (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           uid TEXT NOT NULL,
           account TEXT NOT NULL,
           sender TEXT NOT NULL,
           subject TEXT NOT NULL,
           body TEXT NOT NULL
         );
         CREATE TABLE otp_notifications (
           account TEXT NOT NULL,
           uid TEXT NOT NULL,
           notified_at INTEGER NOT NULL,
           PRIMARY KEY (account, uid)
         );",
    )
    .unwrap();
    drop(conn);

    let error = match Store::open(db.path_str()) {
        Ok(_) => panic!("损坏 schema 必须拒绝"),
        Err(error) => error,
    };
    assert!(matches!(error, StoreOpenError::InvalidSchema(_)));

    let conn = Connection::open(db.path()).unwrap();
    assert_eq!(schema_version(db.path()), 0, "失败迁移不能提升版本");
    let created_indexes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name IN
               ('idx_messages_account_uid', 'idx_messages_account_id')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(created_indexes, 0, "失败迁移创建的索引必须一起回滚");
}

/// 分词器换掉不会报错，只会让所有中文查询静默失效——这种事必须在打开数据库时
/// 就被拦住（ADR-0029 决策 3、10）。
#[test]
fn a_tampered_fts_tokenizer_or_trigger_is_rejected_on_open() {
    for tamper in [
        "UPDATE sqlite_master SET sql = replace(sql, 'unicode61 remove_diacritics 2', 'trigram')
           WHERE name = 'messages_fts';",
        "UPDATE sqlite_master SET sql = replace(sql, 'DELETE FROM thread_refs', 'SELECT 1 FROM thread_refs')
           WHERE name = 'messages_fts_delete';",
        "PRAGMA writable_schema=RESET; DROP TRIGGER messages_fts_delete;",
    ] {
        let db = TempDb::new("fts-tamper");
        drop(Store::open(db.path_str()).expect("先创建完整 schema"));
        let conn = Connection::open(db.path()).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA writable_schema=ON; {tamper} PRAGMA writable_schema=OFF;"
        ))
        .unwrap();
        drop(conn);
        assert!(
            matches!(Store::open(db.path_str()), Err(StoreOpenError::InvalidSchema(_))),
            "被改过的检索对象必须拒绝打开: {tamper}"
        );
    }
}

#[test]
fn malformed_current_schema_is_rejected_without_repair() {
    let db = TempDb::new("malformed-current");
    drop(Store::open(db.path_str()).expect("先创建完整 v1 schema"));

    let conn = Connection::open(db.path()).unwrap();
    conn.execute_batch("DROP INDEX idx_messages_account_id;")
        .unwrap();
    drop(conn);

    let error = match Store::open(db.path_str()) {
        Ok(_) => panic!("损坏的当前 schema 必须拒绝"),
        Err(error) => error,
    };
    assert!(matches!(error, StoreOpenError::InvalidSchema(_)));

    let conn = Connection::open(db.path()).unwrap();
    assert_eq!(schema_version(db.path()), SCHEMA_VERSION);
    let index_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_messages_account_id'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_count, 0, "当前版本损坏时不能静默修补");
}
