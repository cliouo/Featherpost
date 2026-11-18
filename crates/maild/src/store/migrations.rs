use std::error::Error;
use std::fmt;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::DatabaseLockError;

pub const SCHEMA_VERSION: i64 = 10;

/// 恢复输入接受的最低历史版本：v1 备份可进新库，发布后迁移到当前版本。
pub const MIN_SUPPORTED_SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  uid TEXT NOT NULL,
  account TEXT NOT NULL,
  sender TEXT NOT NULL,
  subject TEXT NOT NULL,
  body TEXT NOT NULL,
  received_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_account_uid ON messages(account, uid);
CREATE INDEX IF NOT EXISTS idx_messages_account_id ON messages(account, id DESC);
CREATE TABLE IF NOT EXISTS otp_notifications (
  account TEXT NOT NULL,
  uid TEXT NOT NULL,
  notified_at INTEGER NOT NULL,
  PRIMARY KEY (account, uid)
);
";

/// v1 → v2（ADR-0022）：messages 增收件人列；地址层表。
/// 在 user_version < 2 的立即事务内执行恰好一次；崩溃回滚后下次打开重放。
const SCHEMA_V2: &str = "
ALTER TABLE messages ADD COLUMN recipient TEXT NOT NULL DEFAULT '';
CREATE TABLE IF NOT EXISTS addresses (
  id TEXT PRIMARY KEY NOT NULL,
  account TEXT NOT NULL,
  address TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('alias', 'disposable')),
  label TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL CHECK(state IN ('active', 'disabled', 'expired')),
  created_at INTEGER NOT NULL CHECK(created_at >= 0),
  expires_at INTEGER CHECK(expires_at IS NULL OR expires_at >= 0)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_addresses_address ON addresses(address);
CREATE INDEX IF NOT EXISTS idx_addresses_account ON addresses(account, id);
";

/// v2 → v3（ADR-0025）：messages 补齐 RFC 5322 事实、HTML 原文、摘要与标记位；
/// 新增 folders（结构预置，M4.5-02 首个写者）与 attachments。
///
/// `sort_at` 回填为 `received_at`：v3 之前的行没有 `date_at`，这正是它们的正确排序键，
/// 因此升级不会重排既有列表。
const SCHEMA_V3: &str = "
ALTER TABLE messages ADD COLUMN message_id TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN in_reply_to TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN reference_ids TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN cc TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN bcc TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN reply_to TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN date_at INTEGER;
ALTER TABLE messages ADD COLUMN sort_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN body_html TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN snippet TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN has_attachments INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN seen INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN flagged INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN answered INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;
UPDATE messages SET sort_at = received_at;
CREATE INDEX IF NOT EXISTS idx_messages_sort ON messages(sort_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_messages_account_sort ON messages(account, sort_at DESC, id DESC);
CREATE TABLE IF NOT EXISTS folders (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account TEXT NOT NULL,
  name TEXT NOT NULL,
  role TEXT NOT NULL DEFAULT ''
    CHECK(role IN ('', 'inbox', 'sent', 'drafts', 'trash', 'archive', 'spam')),
  uid_validity INTEGER,
  last_seen_uid INTEGER,
  synced_at INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_folders_account_name ON folders(account, name);
CREATE TABLE IF NOT EXISTS attachments (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account TEXT NOT NULL,
  uid TEXT NOT NULL,
  part_index INTEGER NOT NULL,
  filename TEXT NOT NULL DEFAULT '',
  mime_type TEXT NOT NULL DEFAULT '',
  content_id TEXT NOT NULL DEFAULT '',
  inline INTEGER NOT NULL DEFAULT 0,
  size INTEGER NOT NULL,
  content BLOB
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_attachments_part ON attachments(account, uid, part_index);
CREATE INDEX IF NOT EXISTS idx_attachments_cid ON attachments(account, uid, content_id);
";

/// v3 → v4（ADR-0026）：一封邮件的身份从 `(account, uid)` 变成 `(account, folder_id, uid)`。
///
/// 回填是**断言而不是推断**：v4 之前 receive 只 SELECT 一个 mailbox，委托 provider 也只有
/// 一个信箱，所以历史行确实来自单一信箱，挂到自动创建的 `INBOX` 上。游标同时设为该文件夹
/// 已有的最大数字 UID——升级后不重下收件箱。
const SCHEMA_V4: &str = "
ALTER TABLE folders ADD COLUMN display_name TEXT NOT NULL DEFAULT '';
ALTER TABLE folders ADD COLUMN present INTEGER NOT NULL DEFAULT 1;
ALTER TABLE messages ADD COLUMN folder_id INTEGER NOT NULL DEFAULT 0;
INSERT INTO folders (account, name, role, display_name)
  SELECT DISTINCT account, 'INBOX', 'inbox', 'INBOX' FROM messages;
UPDATE messages SET folder_id = (
  SELECT id FROM folders
  WHERE folders.account = messages.account AND folders.name = 'INBOX'
);
UPDATE folders SET last_seen_uid = (
  SELECT MAX(CAST(uid AS INTEGER)) FROM messages
  WHERE messages.folder_id = folders.id AND messages.uid GLOB '[0-9]*'
) WHERE EXISTS (SELECT 1 FROM messages WHERE messages.folder_id = folders.id);
DROP INDEX IF EXISTS idx_messages_account_uid;
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_account_folder_uid
  ON messages(account, folder_id, uid);
CREATE INDEX IF NOT EXISTS idx_messages_folder_sort
  ON messages(folder_id, sort_at DESC, id DESC);
ALTER TABLE attachments ADD COLUMN folder_id INTEGER NOT NULL DEFAULT 0;
UPDATE attachments SET folder_id = COALESCE((
  SELECT folder_id FROM messages
  WHERE messages.account = attachments.account AND messages.uid = attachments.uid
), 0);
DROP INDEX IF EXISTS idx_attachments_part;
DROP INDEX IF EXISTS idx_attachments_cid;
CREATE UNIQUE INDEX IF NOT EXISTS idx_attachments_folder_part
  ON attachments(account, folder_id, uid, part_index);
CREATE INDEX IF NOT EXISTS idx_attachments_folder_cid
  ON attachments(account, folder_id, uid, content_id);
";

/// v4 → v5（ADR-0027）：写路径与离线队列。
///
/// `flags_local_at` 是对账的时间护栏，`pending_removal` 是乐观移动/删除期间「从源文件夹
/// 消失」的本地表现，`highest_modseq` 支撑 CONDSTORE 短路。
const SCHEMA_V5: &str = "
ALTER TABLE messages ADD COLUMN flags_local_at INTEGER;
ALTER TABLE messages ADD COLUMN pending_removal INTEGER NOT NULL DEFAULT 0;
ALTER TABLE folders ADD COLUMN highest_modseq INTEGER;
CREATE TABLE IF NOT EXISTS pending_ops (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account TEXT NOT NULL,
  folder_id INTEGER NOT NULL,
  uid TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('flag', 'move')),
  payload TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL
    CHECK(state IN ('pending', 'in_flight', 'done', 'failed', 'unknown')),
  attempts INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_pending_ops_account_state
  ON pending_ops(account, state, id);
CREATE INDEX IF NOT EXISTS idx_pending_ops_message
  ON pending_ops(account, folder_id, uid);
";

/// v5 → v6（ADR-0028 决策 9）：`pending_ops.kind` 增加 `'append'`（归档到已发送）。
///
/// SQLite 不能修改 `CHECK`，只能重建表。整个过程在迁移事务内完成，既有 op 逐行拷贝。
const SCHEMA_V6: &str = "
CREATE TABLE pending_ops_v6 (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account TEXT NOT NULL,
  folder_id INTEGER NOT NULL,
  uid TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('flag', 'move', 'append')),
  payload TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL
    CHECK(state IN ('pending', 'in_flight', 'done', 'failed', 'unknown')),
  attempts INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  detail TEXT NOT NULL DEFAULT ''
);
INSERT INTO pending_ops_v6
  (id, account, folder_id, uid, kind, payload, state, attempts, created_at, updated_at, detail)
  SELECT id, account, folder_id, uid, kind, payload, state, attempts, created_at, updated_at, detail
  FROM pending_ops;
DROP TABLE pending_ops;
ALTER TABLE pending_ops_v6 RENAME TO pending_ops;
CREATE INDEX IF NOT EXISTS idx_pending_ops_account_state
  ON pending_ops(account, state, id);
CREATE INDEX IF NOT EXISTS idx_pending_ops_message
  ON pending_ops(account, folder_id, uid);
";

/// FTS5 虚表的建表语句。**分词器是语义的一部分**（ADR-0029 决策 3）：换成别的分词器
/// 会让所有中文查询静默失效，所以这条语句原文进 schema 校验，逐字节比对。
const FTS_TABLE_SQL: &str = "CREATE VIRTUAL TABLE messages_fts USING fts5(\
subject, sender, recipients, body, tokenize = 'unicode61 remove_diacritics 2')";

/// 删除方向由 SQL 兜底（ADR-0029 决策 10）。写入漏了只是搜不到，删除漏了会让搜索
/// 命中一封已经不存在的邮件；删消息的路径有三条，将来还会有第四条。
const FTS_DELETE_TRIGGER_SQL: &str = "CREATE TRIGGER messages_fts_delete \
AFTER DELETE ON messages BEGIN
  DELETE FROM messages_fts WHERE rowid = OLD.id;
  DELETE FROM thread_refs WHERE message_row_id = OLD.id;
END";

/// v6 → v7（ADR-0029）：本地搜索索引与线程聚合。
///
/// **一行历史消息都不在这里索引**：迁移跑在打开数据库的立即事务里，几万封邮件的分词
/// 与索引写入会让 daemon 启动挂起、WAL 膨胀，中途崩溃还要整体回滚重来。这里只记下
/// 回填起点 `MAX(id)`，由后台任务分批推进（决策 9）。空库直接标记完成。
const SCHEMA_V7_TABLES: &str = "
ALTER TABLE messages ADD COLUMN thread_id INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_messages_thread
  ON messages(account, thread_id, sort_at DESC, id DESC);
CREATE TABLE IF NOT EXISTS thread_refs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account TEXT NOT NULL,
  ref_id TEXT NOT NULL,
  message_row_id INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_thread_refs_edge
  ON thread_refs(account, ref_id, message_row_id);
CREATE INDEX IF NOT EXISTS idx_thread_refs_message ON thread_refs(message_row_id);
CREATE TABLE IF NOT EXISTS index_state (
  name TEXT PRIMARY KEY NOT NULL,
  cursor INTEGER NOT NULL,
  done INTEGER NOT NULL DEFAULT 0
);
";

const SCHEMA_V7_STATE: &str = "
INSERT OR REPLACE INTO index_state (name, cursor, done)
  SELECT 'search', COALESCE(MAX(id), 0), CASE WHEN MAX(id) IS NULL THEN 1 ELSE 0 END
  FROM messages;
";

/// 虚表与触发器不能写进 `IF NOT EXISTS` 常量里做逐字节校验，因此整批在这里拼装：
/// 迁移执行的语句与校验比对的语句是**同一个**字符串常量。
fn schema_v7() -> String {
    format!("{SCHEMA_V7_TABLES}{FTS_TABLE_SQL};\n{FTS_DELETE_TRIGGER_SQL};\n{SCHEMA_V7_STATE}")
}

/// v7 → v8（ADR-0034）：daemon 拥有的持久草稿。
///
/// 纯新增两张表，既有表一列不动。草稿进邮件库而不是库外文件，是与阅读策略
/// （ADR-0032 决策 5）相反的选择：草稿是用户写的内容，恢复一份旧备份就该把当时
/// 那封没写完的信一起带回来（ADR-0034 决策 2）。
///
/// 收件人列存 JSON 数组——显示名形式里本来就有逗号，用逗号当分隔符等于自埋
/// 「收件人被劈成两个」的 bug。附件引用存文件夹名而不是 `folder_id`：API 与 CLI
/// 讲的是名字，而改名会让两种存法同样悬空。
const SCHEMA_V8: &str = "
CREATE TABLE IF NOT EXISTS drafts (
  id TEXT PRIMARY KEY NOT NULL,
  account TEXT NOT NULL,
  from_address TEXT NOT NULL DEFAULT '',
  to_addrs TEXT NOT NULL DEFAULT '[]',
  cc_addrs TEXT NOT NULL DEFAULT '[]',
  bcc_addrs TEXT NOT NULL DEFAULT '[]',
  subject TEXT NOT NULL DEFAULT '',
  body TEXT NOT NULL DEFAULT '',
  body_html TEXT NOT NULL DEFAULT '',
  in_reply_to TEXT NOT NULL DEFAULT '',
  reference_ids TEXT NOT NULL DEFAULT '',
  version INTEGER NOT NULL DEFAULT 1 CHECK(version >= 1),
  created_at INTEGER NOT NULL CHECK(created_at >= 0),
  updated_at INTEGER NOT NULL CHECK(updated_at >= 0)
);
CREATE INDEX IF NOT EXISTS idx_drafts_account_updated
  ON drafts(account, updated_at DESC, id);
CREATE TABLE IF NOT EXISTS draft_attachments (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  draft_id TEXT NOT NULL,
  position INTEGER NOT NULL CHECK(position >= 0),
  account TEXT NOT NULL,
  folder TEXT NOT NULL,
  uid TEXT NOT NULL,
  part_index INTEGER NOT NULL CHECK(part_index >= 0),
  filename TEXT NOT NULL DEFAULT '',
  mime_type TEXT NOT NULL DEFAULT '',
  size INTEGER NOT NULL DEFAULT 0 CHECK(size >= 0)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_draft_attachments_slot
  ON draft_attachments(draft_id, position);
";

const SCHEMA_V9: &str = "
ALTER TABLE draft_attachments ADD COLUMN upload_id TEXT;
ALTER TABLE draft_attachments ADD COLUMN upload_owner TEXT;
ALTER TABLE draft_attachments ADD COLUMN content BLOB;
CREATE UNIQUE INDEX idx_draft_attachments_upload ON draft_attachments(upload_id);
";

const SCHEMA_V10: &str = "
ALTER TABLE drafts ADD COLUMN send_state TEXT NOT NULL DEFAULT 'editing'
 CHECK(send_state IN ('editing','failed','sending','unknown','sent'));
ALTER TABLE drafts ADD COLUMN sent_archive INTEGER;
";

#[derive(Debug)]
pub enum StoreOpenError {
    Lock(DatabaseLockError),
    Database(rusqlite::Error),
    UnsupportedSchemaVersion { found: i64, supported: i64 },
    InvalidSchema(String),
}

impl fmt::Display for StoreOpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lock(error) => write!(f, "{error}"),
            Self::Database(error) => write!(f, "SQLite 错误: {error}"),
            Self::UnsupportedSchemaVersion { found, supported } => {
                write!(f, "数据库 schema 版本 {found} 高于当前支持版本 {supported}")
            }
            Self::InvalidSchema(detail) => {
                write!(f, "数据库 schema 校验失败: {detail}")
            }
        }
    }
}

impl Error for StoreOpenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Lock(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::UnsupportedSchemaVersion { .. } | Self::InvalidSchema(_) => None,
        }
    }
}

impl From<rusqlite::Error> for StoreOpenError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<DatabaseLockError> for StoreOpenError {
    fn from(error: DatabaseLockError) -> Self {
        Self::Lock(error)
    }
}

pub(super) fn prepare(conn: &mut Connection) -> Result<(), StoreOpenError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = schema_version(&tx)?;
    let migrated = version < SCHEMA_VERSION;
    if version > SCHEMA_VERSION {
        return Err(StoreOpenError::UnsupportedSchemaVersion {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }

    if version == 0 {
        tx.execute_batch(SCHEMA_V1)?;
    }
    if version < 2 {
        tx.execute_batch(SCHEMA_V2)?;
    }
    if version < 3 {
        // v3 的 sort_at 回填读 v2 的列。先精确校验来源结构，使畸形历史库报告
        // schema 错误而不是含混的 SQLite「列不存在」。
        validate_schema_v2(&tx)?;
        tx.execute_batch(SCHEMA_V3)?;
    }
    if version < 4 {
        validate_schema_v3(&tx)?;
        tx.execute_batch(SCHEMA_V4)?;
    }
    if version < 5 {
        validate_schema_v4(&tx)?;
        tx.execute_batch(SCHEMA_V5)?;
    }
    if version < 6 {
        validate_schema_v5(&tx)?;
        tx.execute_batch(SCHEMA_V6)?;
    }
    if version < 7 {
        validate_schema_v6(&tx)?;
        tx.execute_batch(&schema_v7())?;
    }
    if version < 8 {
        validate_schema_v7(&tx)?;
        tx.execute_batch(SCHEMA_V8)?;
    }
    if version < 9 {
        validate_schema_v8(&tx)?;
        tx.execute_batch(SCHEMA_V9)?;
    }
    if version < 10 {
        validate_schema_v9(&tx)?;
        tx.execute_batch(SCHEMA_V10)?;
    }
    validate_schema(&tx)?;
    if version < SCHEMA_VERSION {
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    tx.commit()?;
    // `PRAGMA journal_mode` 会返回一行；用 `query_row` 读完它，语句才被完全重置。
    // 内存库返回 "memory" 而不是 "wal"，因此这里不断言取值。
    let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if migrated {
        // 迁移（尤其是 v6 的 pending_ops 表重建）会把整张表写进 WAL。就地 checkpoint
        // 一次，让 `prepare` 的后置条件是「WAL 已排空」——否则紧接着做 TRUNCATE
        // checkpoint 的调用方（恢复路径的候选库校验）会撞上 SQLITE_LOCKED。
        // 尽力而为：checkpoint 失败不是拒绝打开数据库的理由。
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            row.get::<_, i64>(0)
        });
    }
    Ok(())
}

/// 文件型数据库的低占用运行时参数（ADR-0041 决策 5）。
///
/// 只影响连接的运行时内存与 WAL 截断上限，不触碰任何表 / 索引 schema：
/// - `cache_size = -1024`：页缓存约 1 MiB（默认 2 MiB，且随连接常驻）；
/// - `temp_store = FILE`：排序 / 临时 B 树落盘而不是吃堆内存；
/// - `journal_size_limit = 4 MiB`：checkpoint 后 WAL 截回到 4 MiB 以内，不随一次大写入永久膨胀；
/// - `mmap_size = 0`：不把库文件映射进地址空间（映射页会计入 RSS / phys_footprint）。
///
/// 尽力而为：个别 PRAGMA 不被支持不是拒绝打开数据库的理由。
pub(super) fn apply_low_footprint_pragmas(conn: &Connection) {
    for pragma in [
        "PRAGMA cache_size = -1024",
        "PRAGMA temp_store = FILE",
        "PRAGMA journal_size_limit = 4194304",
        "PRAGMA mmap_size = 0",
    ] {
        // 部分 PRAGMA 会回一行当前值：逐行读完，语句才被完全重置。
        let _ = conn.prepare(pragma).and_then(|mut statement| {
            let mut rows = statement.query([])?;
            while rows.next()?.is_some() {}
            Ok(())
        });
    }
}

fn schema_version(conn: &Connection) -> Result<i64, rusqlite::Error> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
}

pub(super) fn validate_existing(conn: &Connection) -> Result<(), StoreOpenError> {
    let version = schema_version(conn)?;
    if version > SCHEMA_VERSION {
        return Err(StoreOpenError::UnsupportedSchemaVersion {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version < SCHEMA_VERSION {
        return Err(StoreOpenError::InvalidSchema(format!(
            "数据库 schema 版本为 {version}，备份要求当前版本 {SCHEMA_VERSION}"
        )));
    }
    validate_schema(conn)
}

#[derive(Debug, Eq, PartialEq)]
struct Column {
    name: String,
    kind: String,
    not_null: bool,
    primary_key_position: i64,
}

impl Column {
    fn new(name: &str, kind: &str, not_null: bool, primary_key_position: i64) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            not_null,
            primary_key_position,
        }
    }
}

/// v1 的 messages 列（顺序即 cid 顺序）。后续版本在其尾部追加。
fn messages_columns_v1() -> Vec<Column> {
    vec![
        Column::new("id", "INTEGER", false, 1),
        Column::new("uid", "TEXT", true, 0),
        Column::new("account", "TEXT", true, 0),
        Column::new("sender", "TEXT", true, 0),
        Column::new("subject", "TEXT", true, 0),
        Column::new("body", "TEXT", true, 0),
        Column::new("received_at", "INTEGER", true, 0),
    ]
}

/// v2（ADR-0022）：ALTER TABLE ADD COLUMN，旧行回填空串。
fn messages_columns_v2() -> Vec<Column> {
    let mut columns = messages_columns_v1();
    columns.push(Column::new("recipient", "TEXT", true, 0));
    columns
}

/// v3（ADR-0025）：RFC 5322 事实、HTML 原文、摘要与标记位。
fn messages_columns_v3() -> Vec<Column> {
    let mut columns = messages_columns_v2();
    columns.extend([
        Column::new("message_id", "TEXT", true, 0),
        Column::new("in_reply_to", "TEXT", true, 0),
        Column::new("reference_ids", "TEXT", true, 0),
        Column::new("cc", "TEXT", true, 0),
        Column::new("bcc", "TEXT", true, 0),
        Column::new("reply_to", "TEXT", true, 0),
        Column::new("date_at", "INTEGER", false, 0),
        Column::new("sort_at", "INTEGER", true, 0),
        Column::new("body_html", "TEXT", true, 0),
        Column::new("snippet", "TEXT", true, 0),
        Column::new("has_attachments", "INTEGER", true, 0),
        Column::new("seen", "INTEGER", true, 0),
        Column::new("flagged", "INTEGER", true, 0),
        Column::new("answered", "INTEGER", true, 0),
        Column::new("deleted", "INTEGER", true, 0),
    ]);
    columns
}

/// v4（ADR-0026）：一封邮件的身份变成 `(account, folder_id, uid)`。
fn messages_columns_v4() -> Vec<Column> {
    let mut columns = messages_columns_v3();
    columns.push(Column::new("folder_id", "INTEGER", true, 0));
    columns
}

/// v3 的 folders 形状（结构预置，尚无写者）。
fn folders_columns_v3() -> Vec<Column> {
    vec![
        Column::new("id", "INTEGER", false, 1),
        Column::new("account", "TEXT", true, 0),
        Column::new("name", "TEXT", true, 0),
        Column::new("role", "TEXT", true, 0),
        Column::new("uid_validity", "INTEGER", false, 0),
        Column::new("last_seen_uid", "INTEGER", false, 0),
        Column::new("synced_at", "INTEGER", false, 0),
    ]
}

fn folders_columns_v4() -> Vec<Column> {
    let mut columns = folders_columns_v3();
    columns.extend([
        Column::new("display_name", "TEXT", true, 0),
        Column::new("present", "INTEGER", true, 0),
    ]);
    columns
}

/// v5（ADR-0027）：CONDSTORE 短路用的 `HIGHESTMODSEQ`。
fn folders_columns_v5() -> Vec<Column> {
    let mut columns = folders_columns_v4();
    columns.push(Column::new("highest_modseq", "INTEGER", false, 0));
    columns
}

/// v5：对账时间护栏与乐观移除标记。
fn messages_columns_v5() -> Vec<Column> {
    let mut columns = messages_columns_v4();
    columns.extend([
        Column::new("flags_local_at", "INTEGER", false, 0),
        Column::new("pending_removal", "INTEGER", true, 0),
    ]);
    columns
}

/// v7（ADR-0029）：线程聚合的物化列。
fn messages_columns_v7() -> Vec<Column> {
    let mut columns = messages_columns_v5();
    columns.push(Column::new("thread_id", "INTEGER", true, 0));
    columns
}

fn thread_refs_columns() -> Vec<Column> {
    vec![
        Column::new("id", "INTEGER", false, 1),
        Column::new("account", "TEXT", true, 0),
        Column::new("ref_id", "TEXT", true, 0),
        Column::new("message_row_id", "INTEGER", true, 0),
    ]
}

fn index_state_columns() -> Vec<Column> {
    vec![
        Column::new("name", "TEXT", true, 1),
        Column::new("cursor", "INTEGER", true, 0),
        Column::new("done", "INTEGER", true, 0),
    ]
}

/// v8（ADR-0034）：草稿行。收件人是 JSON 数组文本，`version` 是并发仲裁。
fn drafts_columns() -> Vec<Column> {
    vec![
        Column::new("id", "TEXT", true, 1),
        Column::new("account", "TEXT", true, 0),
        Column::new("from_address", "TEXT", true, 0),
        Column::new("to_addrs", "TEXT", true, 0),
        Column::new("cc_addrs", "TEXT", true, 0),
        Column::new("bcc_addrs", "TEXT", true, 0),
        Column::new("subject", "TEXT", true, 0),
        Column::new("body", "TEXT", true, 0),
        Column::new("body_html", "TEXT", true, 0),
        Column::new("in_reply_to", "TEXT", true, 0),
        Column::new("reference_ids", "TEXT", true, 0),
        Column::new("version", "INTEGER", true, 0),
        Column::new("created_at", "INTEGER", true, 0),
        Column::new("updated_at", "INTEGER", true, 0),
    ]
}

/// v8：草稿的附件引用。只指向库里已有的 part，字节不复制（ADR-0034 决策 6）。
fn draft_attachments_columns() -> Vec<Column> {
    vec![
        Column::new("id", "INTEGER", false, 1),
        Column::new("draft_id", "TEXT", true, 0),
        Column::new("position", "INTEGER", true, 0),
        Column::new("account", "TEXT", true, 0),
        Column::new("folder", "TEXT", true, 0),
        Column::new("uid", "TEXT", true, 0),
        Column::new("part_index", "INTEGER", true, 0),
        Column::new("filename", "TEXT", true, 0),
        Column::new("mime_type", "TEXT", true, 0),
        Column::new("size", "INTEGER", true, 0),
    ]
}

fn pending_ops_columns() -> Vec<Column> {
    vec![
        Column::new("id", "INTEGER", false, 1),
        Column::new("account", "TEXT", true, 0),
        Column::new("folder_id", "INTEGER", true, 0),
        Column::new("uid", "TEXT", true, 0),
        Column::new("kind", "TEXT", true, 0),
        Column::new("payload", "TEXT", true, 0),
        Column::new("state", "TEXT", true, 0),
        Column::new("attempts", "INTEGER", true, 0),
        Column::new("created_at", "INTEGER", true, 0),
        Column::new("updated_at", "INTEGER", true, 0),
        Column::new("detail", "TEXT", true, 0),
    ]
}

fn attachments_columns_v3() -> Vec<Column> {
    vec![
        Column::new("id", "INTEGER", false, 1),
        Column::new("account", "TEXT", true, 0),
        Column::new("uid", "TEXT", true, 0),
        Column::new("part_index", "INTEGER", true, 0),
        Column::new("filename", "TEXT", true, 0),
        Column::new("mime_type", "TEXT", true, 0),
        Column::new("content_id", "TEXT", true, 0),
        Column::new("inline", "INTEGER", true, 0),
        Column::new("size", "INTEGER", true, 0),
        Column::new("content", "BLOB", false, 0),
    ]
}

/// v4：附件也随文件夹分维——否则收件箱与已发送里同号 UID 的附件会互相顶掉。
fn attachments_columns_v4() -> Vec<Column> {
    let mut columns = attachments_columns_v3();
    columns.push(Column::new("folder_id", "INTEGER", true, 0));
    columns
}

fn otp_notifications_columns() -> Vec<Column> {
    vec![
        Column::new("account", "TEXT", true, 1),
        Column::new("uid", "TEXT", true, 2),
        Column::new("notified_at", "INTEGER", true, 0),
    ]
}

fn addresses_columns() -> Vec<Column> {
    vec![
        Column::new("id", "TEXT", true, 1),
        Column::new("account", "TEXT", true, 0),
        Column::new("address", "TEXT", true, 0),
        Column::new("kind", "TEXT", true, 0),
        Column::new("label", "TEXT", true, 0),
        Column::new("state", "TEXT", true, 0),
        Column::new("created_at", "INTEGER", true, 0),
        Column::new("expires_at", "INTEGER", false, 0),
    ]
}

/// v1 起一直存在的 messages 索引。
fn validate_messages_account_id_index(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_index(
        conn,
        "messages",
        "idx_messages_account_id",
        false,
        &[("account", false), ("id", true)],
    )
}

/// v1–v3 的 `(account, uid)` 唯一索引。v4 换成 `(account, folder_id, uid)`（ADR-0026 决策 1）。
fn validate_messages_indexes(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_index(
        conn,
        "messages",
        "idx_messages_account_uid",
        true,
        &[("account", false), ("uid", false)],
    )?;
    validate_messages_account_id_index(conn)
}

/// v3 起的排序索引。
fn validate_messages_sort_indexes(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_index(
        conn,
        "messages",
        "idx_messages_sort",
        false,
        &[("sort_at", true), ("id", true)],
    )?;
    validate_index(
        conn,
        "messages",
        "idx_messages_account_sort",
        false,
        &[("account", false), ("sort_at", true), ("id", true)],
    )
}

fn validate_attachments_indexes_v3(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_index(
        conn,
        "attachments",
        "idx_attachments_part",
        true,
        &[("account", false), ("uid", false), ("part_index", false)],
    )?;
    validate_index(
        conn,
        "attachments",
        "idx_attachments_cid",
        false,
        &[("account", false), ("uid", false), ("content_id", false)],
    )
}

/// v4 起：唯一键从 `(account, uid)` 换成 `(account, folder_id, uid)`。
fn validate_messages_folder_indexes(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_index(
        conn,
        "messages",
        "idx_messages_account_folder_uid",
        true,
        &[("account", false), ("folder_id", false), ("uid", false)],
    )?;
    validate_index(
        conn,
        "messages",
        "idx_messages_folder_sort",
        false,
        &[("folder_id", false), ("sort_at", true), ("id", true)],
    )?;
    if index_exists(conn, "idx_messages_account_uid")? {
        return Err(StoreOpenError::InvalidSchema(
            "v4 起不应保留 (account, uid) 唯一索引：跨文件夹同 UID 会被误判为同一封".into(),
        ));
    }
    Ok(())
}

fn validate_attachments_indexes_v4(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_index(
        conn,
        "attachments",
        "idx_attachments_folder_part",
        true,
        &[
            ("account", false),
            ("folder_id", false),
            ("uid", false),
            ("part_index", false),
        ],
    )?;
    validate_index(
        conn,
        "attachments",
        "idx_attachments_folder_cid",
        false,
        &[
            ("account", false),
            ("folder_id", false),
            ("uid", false),
            ("content_id", false),
        ],
    )
}

fn validate_addresses_indexes(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_index(
        conn,
        "addresses",
        "idx_addresses_address",
        true,
        &[("address", false)],
    )?;
    validate_index(
        conn,
        "addresses",
        "idx_addresses_account",
        false,
        &[("account", false), ("id", false)],
    )
}

fn validate_folders_index(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_index(
        conn,
        "folders",
        "idx_folders_account_name",
        true,
        &[("account", false), ("name", false)],
    )
}

fn validate_schema_v3(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_table(conn, "messages", &messages_columns_v3())?;
    validate_table(conn, "otp_notifications", &otp_notifications_columns())?;
    validate_table(conn, "addresses", &addresses_columns())?;
    validate_table(conn, "folders", &folders_columns_v3())?;
    validate_table(conn, "attachments", &attachments_columns_v3())?;
    validate_messages_indexes(conn)?;
    validate_messages_sort_indexes(conn)?;
    validate_addresses_indexes(conn)?;
    validate_folders_index(conn)?;
    validate_attachments_indexes_v3(conn)?;
    reject_reserved_objects(conn, RESERVED_V4_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V5_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V7_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V8_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V9_OBJECTS)
}

fn validate_schema_v4(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_table(conn, "messages", &messages_columns_v4())?;
    validate_table(conn, "otp_notifications", &otp_notifications_columns())?;
    validate_table(conn, "addresses", &addresses_columns())?;
    validate_table(conn, "folders", &folders_columns_v4())?;
    validate_table(conn, "attachments", &attachments_columns_v4())?;
    validate_messages_account_id_index(conn)?;
    validate_messages_sort_indexes(conn)?;
    validate_messages_folder_indexes(conn)?;
    validate_addresses_indexes(conn)?;
    validate_folders_index(conn)?;
    validate_attachments_indexes_v4(conn)?;
    reject_reserved_objects(conn, RESERVED_V5_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V7_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V8_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V9_OBJECTS)
}

fn validate_schema_v5(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_table(conn, "messages", &messages_columns_v5())?;
    validate_table(conn, "otp_notifications", &otp_notifications_columns())?;
    validate_table(conn, "addresses", &addresses_columns())?;
    validate_table(conn, "folders", &folders_columns_v5())?;
    validate_table(conn, "attachments", &attachments_columns_v4())?;
    validate_table(conn, "pending_ops", &pending_ops_columns())?;
    validate_messages_account_id_index(conn)?;
    validate_messages_sort_indexes(conn)?;
    validate_messages_folder_indexes(conn)?;
    validate_addresses_indexes(conn)?;
    validate_folders_index(conn)?;
    validate_attachments_indexes_v4(conn)?;
    validate_pending_ops_indexes(conn)?;
    reject_reserved_objects(conn, RESERVED_V7_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V8_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V9_OBJECTS)
}

fn validate_pending_ops_indexes(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_index(
        conn,
        "pending_ops",
        "idx_pending_ops_account_state",
        false,
        &[("account", false), ("state", false), ("id", false)],
    )?;
    validate_index(
        conn,
        "pending_ops",
        "idx_pending_ops_message",
        false,
        &[("account", false), ("folder_id", false), ("uid", false)],
    )
}

fn validate_schema_v6(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_table(conn, "messages", &messages_columns_v5())?;
    validate_table(conn, "otp_notifications", &otp_notifications_columns())?;
    validate_table(conn, "addresses", &addresses_columns())?;
    validate_table(conn, "folders", &folders_columns_v5())?;
    validate_table(conn, "attachments", &attachments_columns_v4())?;
    validate_table(conn, "pending_ops", &pending_ops_columns())?;
    validate_messages_account_id_index(conn)?;
    validate_messages_sort_indexes(conn)?;
    validate_messages_folder_indexes(conn)?;
    validate_addresses_indexes(conn)?;
    validate_folders_index(conn)?;
    validate_attachments_indexes_v4(conn)?;
    validate_pending_ops_indexes(conn)?;
    reject_reserved_objects(conn, RESERVED_V7_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V8_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V9_OBJECTS)
}

/// v7 的完整结构。v8 在它之上只加两张表，因此两条路径共用这一段。
fn validate_schema_core_v7(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_table(conn, "messages", &messages_columns_v7())?;
    validate_table(conn, "otp_notifications", &otp_notifications_columns())?;
    validate_table(conn, "addresses", &addresses_columns())?;
    validate_table(conn, "folders", &folders_columns_v5())?;
    validate_table(conn, "attachments", &attachments_columns_v4())?;
    validate_table(conn, "pending_ops", &pending_ops_columns())?;
    validate_table(conn, "thread_refs", &thread_refs_columns())?;
    validate_table(conn, "index_state", &index_state_columns())?;
    validate_messages_account_id_index(conn)?;
    validate_messages_sort_indexes(conn)?;
    validate_messages_folder_indexes(conn)?;
    validate_addresses_indexes(conn)?;
    validate_folders_index(conn)?;
    validate_attachments_indexes_v4(conn)?;
    validate_pending_ops_indexes(conn)?;
    validate_index(
        conn,
        "messages",
        "idx_messages_thread",
        false,
        &[
            ("account", false),
            ("thread_id", false),
            ("sort_at", true),
            ("id", true),
        ],
    )?;
    validate_index(
        conn,
        "thread_refs",
        "idx_thread_refs_edge",
        true,
        &[("account", false), ("ref_id", false), ("message_row_id", false)],
    )?;
    validate_index(
        conn,
        "thread_refs",
        "idx_thread_refs_message",
        false,
        &[("message_row_id", false)],
    )?;
    // 分词器与触发器体是语义的一部分：列名对上不代表行为对上（ADR-0029 决策 3、10）。
    validate_object_sql(conn, "messages_fts", FTS_TABLE_SQL)?;
    validate_object_sql(conn, "messages_fts_delete", FTS_DELETE_TRIGGER_SQL)?;
    for shadow in FTS_SHADOW_TABLES {
        if !object_exists(conn, shadow)? {
            return Err(StoreOpenError::InvalidSchema(format!(
                "缺少 FTS5 影子表 {shadow}"
            )));
        }
    }
    Ok(())
}

fn validate_schema_v7(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_schema_core_v7(conn)?;
    reject_reserved_objects(conn, RESERVED_V8_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V9_OBJECTS)
}

/// v8（ADR-0034）：草稿两张表。
fn validate_schema_core_v8(
    conn: &Connection,
    columns: &[Column],
    draft_columns: &[Column],
) -> Result<(), StoreOpenError> {
    validate_schema_core_v7(conn)?;
    validate_table(conn, "drafts", draft_columns)?;
    validate_table(conn, "draft_attachments", columns)?;
    validate_index(
        conn,
        "drafts",
        "idx_drafts_account_updated",
        false,
        &[("account", false), ("updated_at", true), ("id", false)],
    )?;
    validate_index(
        conn,
        "draft_attachments",
        "idx_draft_attachments_slot",
        true,
        &[("draft_id", false), ("position", false)],
    )
}

fn validate_schema_v8(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_schema_core_v8(conn, &draft_attachments_columns(), &drafts_columns())?;
    reject_reserved_objects(conn, RESERVED_V9_OBJECTS)
}

fn validate_schema_core_v9(
    conn: &Connection,
    draft_columns: &[Column],
) -> Result<(), StoreOpenError> {
    let mut columns = draft_attachments_columns();
    columns.extend([
        Column::new("upload_id", "TEXT", false, 0),
        Column::new("upload_owner", "TEXT", false, 0),
        Column::new("content", "BLOB", false, 0),
    ]);
    validate_schema_core_v8(conn, &columns, draft_columns)?;
    validate_index(
        conn,
        "draft_attachments",
        "idx_draft_attachments_upload",
        true,
        &[("upload_id", false)],
    )
}

fn validate_schema_v9(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_schema_core_v9(conn, &drafts_columns())
}
fn validate_schema(conn: &Connection) -> Result<(), StoreOpenError> {
    let mut columns = drafts_columns();
    columns.extend([
        Column::new("send_state", "TEXT", true, 0),
        Column::new("sent_archive", "INTEGER", false, 0),
    ]);
    validate_schema_core_v9(conn, &columns)
}

const RESERVED_V9_OBJECTS: &[&str] = &["idx_draft_attachments_upload"];

/// FTS5 自建的影子表。它们不是我们写的，但缺一个就说明虚表被外部改过。
const FTS_SHADOW_TABLES: &[&str] = &[
    "messages_fts_data",
    "messages_fts_idx",
    "messages_fts_content",
    "messages_fts_docsize",
    "messages_fts_config",
];

fn object_exists(conn: &Connection, name: &str) -> Result<bool, StoreOpenError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)",
        [name],
        |row| row.get::<_, bool>(0),
    )?)
}

/// 逐字节比对 `sqlite_master` 里的建表/建触发器语句原文。
fn validate_object_sql(
    conn: &Connection,
    name: &str,
    expected: &str,
) -> Result<(), StoreOpenError> {
    let actual = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = ?1",
            [name],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    match actual {
        Some(sql) if sql == expected => Ok(()),
        Some(sql) => Err(StoreOpenError::InvalidSchema(format!(
            "对象 {name} 定义不匹配：期望 {expected:?}，实际 {sql:?}"
        ))),
        None => Err(StoreOpenError::InvalidSchema(format!("缺少对象 {name}"))),
    }
}

fn index_exists(conn: &Connection, name: &str) -> Result<bool, StoreOpenError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1)",
        [name],
        |row| row.get::<_, bool>(0),
    )?)
}

/// 恢复输入校验：接受 [MIN_SUPPORTED_SCHEMA_VERSION, SCHEMA_VERSION] 的历史
/// 备份。每个可恢复版本都必须先通过其精确结构校验；调用方随后只在临时
/// replacement 上迁移并复验当前 schema，绝不让畸形输入先覆盖正式数据库。
pub(super) fn validate_restore_input(conn: &Connection) -> Result<(), StoreOpenError> {
    let version = schema_version(conn)?;
    if version > SCHEMA_VERSION {
        return Err(StoreOpenError::UnsupportedSchemaVersion {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version < MIN_SUPPORTED_SCHEMA_VERSION {
        return Err(StoreOpenError::InvalidSchema(format!(
            "备份 schema 版本 {version} 低于最低可恢复版本 {MIN_SUPPORTED_SCHEMA_VERSION}"
        )));
    }
    match version {
        1 => validate_schema_v1(conn),
        2 => validate_schema_v2(conn),
        3 => validate_schema_v3(conn),
        4 => validate_schema_v4(conn),
        5 => validate_schema_v5(conn),
        6 => validate_schema_v6(conn),
        7 => validate_schema_v7(conn),
        8 => validate_schema_v8(conn),
        9 => validate_schema_v9(conn),
        SCHEMA_VERSION => validate_schema(conn),
        _ => Err(StoreOpenError::InvalidSchema(format!(
            "备份 schema 版本 {version} 不在可恢复集合中"
        ))),
    }
}

/// 更高版本自有对象若已出现在低版本输入里，会让 `CREATE IF NOT EXISTS` 绕过约束定义；
/// 迁移前必须拒绝这些保留名称。
fn reject_reserved_objects(conn: &Connection, reserved: &[&str]) -> Result<(), StoreOpenError> {
    for name in reserved {
        let exists = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)",
            [name],
            |row| row.get::<_, bool>(0),
        )?;
        if exists {
            return Err(StoreOpenError::InvalidSchema(format!(
                "低版本 schema 不应包含保留对象 {name}"
            )));
        }
    }
    Ok(())
}

const RESERVED_V2_OBJECTS: &[&str] = &[
    "addresses",
    "idx_addresses_address",
    "idx_addresses_account",
];

const RESERVED_V3_OBJECTS: &[&str] = &[
    "folders",
    "idx_folders_account_name",
    "attachments",
    "idx_attachments_part",
    "idx_attachments_cid",
    "idx_messages_sort",
    "idx_messages_account_sort",
];

const RESERVED_V5_OBJECTS: &[&str] = &[
    "pending_ops",
    "idx_pending_ops_account_state",
    "idx_pending_ops_message",
];

const RESERVED_V7_OBJECTS: &[&str] = &[
    "messages_fts",
    "messages_fts_data",
    "messages_fts_idx",
    "messages_fts_content",
    "messages_fts_docsize",
    "messages_fts_config",
    "messages_fts_delete",
    "thread_refs",
    "idx_thread_refs_edge",
    "idx_thread_refs_message",
    "index_state",
    "idx_messages_thread",
];

const RESERVED_V8_OBJECTS: &[&str] = &[
    "drafts",
    "idx_drafts_account_updated",
    "draft_attachments",
    "idx_draft_attachments_slot",
];

const RESERVED_V4_OBJECTS: &[&str] = &[
    "idx_messages_account_folder_uid",
    "idx_messages_folder_sort",
    "idx_attachments_folder_part",
    "idx_attachments_folder_cid",
];

fn validate_schema_v2(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_table(conn, "messages", &messages_columns_v2())?;
    validate_table(conn, "otp_notifications", &otp_notifications_columns())?;
    validate_table(conn, "addresses", &addresses_columns())?;
    validate_messages_indexes(conn)?;
    validate_addresses_indexes(conn)?;
    reject_reserved_objects(conn, RESERVED_V3_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V4_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V5_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V7_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V8_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V9_OBJECTS)
}

fn validate_schema_v1(conn: &Connection) -> Result<(), StoreOpenError> {
    validate_table(conn, "messages", &messages_columns_v1())?;
    validate_table(conn, "otp_notifications", &otp_notifications_columns())?;
    validate_messages_indexes(conn)?;
    reject_reserved_objects(conn, RESERVED_V2_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V3_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V4_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V5_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V7_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V8_OBJECTS)?;
    reject_reserved_objects(conn, RESERVED_V9_OBJECTS)
}

fn validate_table(
    conn: &Connection,
    table: &str,
    expected: &[Column],
) -> Result<(), StoreOpenError> {
    let mut stmt = conn.prepare(
        "SELECT name, type, \"notnull\", pk
         FROM pragma_table_info(?1)
         ORDER BY cid",
    )?;
    let actual = stmt
        .query_map([table], |row| {
            Ok(Column {
                name: row.get(0)?,
                kind: row.get(1)?,
                not_null: row.get::<_, i64>(2)? != 0,
                primary_key_position: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if actual != expected {
        return Err(StoreOpenError::InvalidSchema(format!(
            "表 {table} 字段不匹配：期望 {expected:?}，实际 {actual:?}"
        )));
    }
    Ok(())
}

fn validate_index(
    conn: &Connection,
    table: &str,
    index: &str,
    expected_unique: bool,
    expected_columns: &[(&str, bool)],
) -> Result<(), StoreOpenError> {
    let metadata = conn.query_row(
        "SELECT \"unique\", partial
         FROM pragma_index_list(?1)
         WHERE name = ?2",
        [table, index],
        |row| Ok((row.get::<_, i64>(0)? != 0, row.get::<_, i64>(1)? != 0)),
    );
    let (actual_unique, partial) = match metadata {
        Ok(metadata) => metadata,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(StoreOpenError::InvalidSchema(format!("缺少索引 {index}")))
        }
        Err(error) => return Err(error.into()),
    };
    if actual_unique != expected_unique || partial {
        return Err(StoreOpenError::InvalidSchema(format!(
            "索引 {index} 属性不匹配：unique={actual_unique}, partial={partial}"
        )));
    }

    let mut stmt = conn.prepare(
        "SELECT name, \"desc\"
         FROM pragma_index_xinfo(?1)
         WHERE key = 1
         ORDER BY seqno",
    )?;
    let actual_columns = stmt
        .query_map([index], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected_columns = expected_columns
        .iter()
        .map(|(name, descending)| ((*name).to_owned(), *descending))
        .collect::<Vec<_>>();
    if actual_columns != expected_columns {
        return Err(StoreOpenError::InvalidSchema(format!(
            "索引 {index} 字段不匹配：期望 {expected_columns:?}，实际 {actual_columns:?}"
        )));
    }
    Ok(())
}
