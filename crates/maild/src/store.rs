//! store：消息持久化（rusqlite，WAL）。
//!
//! 服务：`Store`——messages 表 + 查询。插件：监听 `MailFiltered` 落库。
//! 维护入口：`backup_database` 用 SQLite Online Backup API 生成经过校验的快照。
//! 插入用 `INSERT OR IGNORE`（(account, uid) 唯一）——receive 重连重放安全。

use std::any::TypeId;
use std::sync::{Arc, Mutex};

use cordis::{Context, Plugin, PluginError, Service};
use rusqlite::{params, types::Type, Connection, OptionalExtension, TransactionBehavior};
use serde::Deserialize;

use crate::addresses::{AddressKind, AddressRecord, AddressState};
use crate::drafts::{
    Draft, DraftAttachmentInput, DraftAttachmentRef, DraftPatch, DraftSummary,
    MAX_DRAFTS_PER_ACCOUNT,
};
use crate::events::{MailAttachment, MailFiltered, MailMessage, MessageSummary};
use crate::folders::{FolderRecord, FolderRole};
use crate::pending::{
    FlagPayload, MessageFlag, MovePayload, ObservedFlags, OpKind, OpState, PendingOp,
    ReconcileOutcome, MAX_AUTO_ATTEMPTS,
};
use crate::loader::Merge;
use crate::mime;
use crate::search::{build_match_expression, segment, thread_ref_ids};

mod backup;
mod lock;
mod migrations;
mod restore;
mod uploads;
mod draft_send;
mod mailbox_queries;
pub use mailbox_queries::{SummaryFilter, WriteReceipt};

pub use backup::{backup_database, BackupError};
pub use lock::{DatabaseLock, DatabaseLockError};
pub use migrations::{StoreOpenError, SCHEMA_VERSION};
pub use restore::{restore_database, RestoreError, RestoreOutcome};

#[derive(Clone, Debug, Deserialize, Default)]
pub struct StoreConfig {
    /// 数据库路径；空字符串 = 内存库（测试用）。
    #[serde(default)]
    pub db_path: String,
}

impl Service for StoreConfig {
    fn name(&self) -> &'static str {
        "store.config"
    }
}

impl Merge for StoreConfig {
    fn merge(&self, fragment: &Self) -> Self {
        fragment.clone()
    }
}

/// 数据库占用快照（ADR-0041 决策 5），供状态观测与基准断言。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreMetrics {
    /// 主库文件字节数（内存库为页数 × 页大小）。
    pub db_bytes: u64,
    /// `-wal` 文件字节数（内存库恒为 0）。
    pub wal_bytes: u64,
    pub message_count: u64,
    /// 库内实际存放的附件字节（只计 `content` 非空的行）。
    pub stored_attachment_bytes: u64,
}

/// 消息存储服务（spine 服务之一）。
pub struct Store {
    conn: Mutex<Connection>,
    uploads: Arc<Mutex<crate::uploads::Uploads>>,
    _database_lock: Option<DatabaseLock>,
    pub db_path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InsertAddressOutcome {
    Inserted,
    Duplicate,
    LimitReached,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PatchAddressOutcome {
    Updated(AddressRecord),
    NotFound,
    Expired,
}

fn invalid_address_column(index: usize, kind: Type, detail: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        kind,
        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, detail)),
    )
}

fn decode_address_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AddressRecord> {
    let kind_value = row.get::<_, String>(3)?;
    let kind = AddressKind::parse(&kind_value).ok_or_else(|| {
        invalid_address_column(3, Type::Text, format!("未知地址 kind: {kind_value}"))
    })?;
    let state_value = row.get::<_, String>(5)?;
    let state = AddressState::parse(&state_value).ok_or_else(|| {
        invalid_address_column(5, Type::Text, format!("未知地址 state: {state_value}"))
    })?;
    let created_at_value = row.get::<_, i64>(6)?;
    let created_at = u64::try_from(created_at_value).map_err(|_| {
        invalid_address_column(
            6,
            Type::Integer,
            format!("地址 created_at 不能为负数: {created_at_value}"),
        )
    })?;
    let expires_at = row
        .get::<_, Option<i64>>(7)?
        .map(|value| {
            u64::try_from(value).map_err(|_| {
                invalid_address_column(
                    7,
                    Type::Integer,
                    format!("地址 expires_at 不能为负数: {value}"),
                )
            })
        })
        .transpose()?;
    Ok(AddressRecord {
        id: row.get(0)?,
        account: row.get(1)?,
        address: row.get(2)?,
        kind,
        label: row.get(4)?,
        state,
        created_at,
        expires_at,
    })
}

/// 消息读取的 FROM 子句。folders 是小表且按 rowid 命中，join 的代价可忽略；
/// 换来的是列表和详情都能直接带上文件夹身份（ADR-0026 决策 10）。
const MESSAGE_FROM: &str = "FROM messages m LEFT JOIN folders f ON f.id = m.folder_id";

/// `messages` 的读取列（不含 `body_html`）。列序即 [`decode_message_row`] 的下标。
const MESSAGE_COLUMNS: &str = "m.uid, m.account, m.sender, m.recipient, m.subject, m.body, \
     m.received_at, m.message_id, m.in_reply_to, m.reference_ids, m.cc, m.bcc, m.reply_to, \
     m.date_at, m.sort_at, m.snippet, m.has_attachments, m.seen, m.flagged, m.answered, \
     m.deleted, COALESCE(f.name, ''), COALESCE(f.role, ''), m.thread_id";

/// `MessageSummary` 的读取列。
const SUMMARY_COLUMNS: &str = "m.uid, m.account, m.sender, m.recipient, m.subject, \
     m.received_at, m.date_at, m.sort_at, m.snippet, m.has_attachments, m.seen, m.flagged, \
     m.answered, m.deleted, COALESCE(f.name, ''), COALESCE(f.role, ''), m.thread_id";

/// 列表排序：可信发信时间在前，同秒时以落库序稳定（ADR-0025 决策 3）。
const MESSAGE_ORDER: &str = "ORDER BY m.sort_at DESC, m.id DESC";

/// 乐观移动/删除期间的行对列表与计数不可见（ADR-0027 决策 5）。
const VISIBLE: &str = "m.pending_removal = 0";

fn decode_message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailMessage> {
    Ok(MailMessage {
        uid: row.get(0)?,
        account: row.get(1)?,
        sender: row.get(2)?,
        recipient: row.get(3)?,
        subject: row.get(4)?,
        body: row.get(5)?,
        received_at: row.get::<_, i64>(6)? as u64,
        message_id: row.get(7)?,
        in_reply_to: row.get(8)?,
        references: row.get(9)?,
        cc: row.get(10)?,
        bcc: row.get(11)?,
        reply_to: row.get(12)?,
        date_at: row.get::<_, Option<i64>>(13)?.map(|value| value as u64),
        sort_at: row.get::<_, i64>(14)? as u64,
        snippet: row.get(15)?,
        has_attachments: row.get(16)?,
        seen: row.get(17)?,
        flagged: row.get(18)?,
        answered: row.get(19)?,
        deleted: row.get(20)?,
        folder: row.get(21)?,
        folder_role: row.get(22)?,
        thread_id: row.get(23)?,
        // 只有 `get_message` 选取 body_html 与附件；列表路径保持轻量。
        body_html: String::new(),
        attachments: Vec::new(),
    })
}

fn decode_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageSummary> {
    Ok(MessageSummary {
        uid: row.get(0)?,
        account: row.get(1)?,
        sender: row.get(2)?,
        recipient: row.get(3)?,
        subject: row.get(4)?,
        received_at: row.get::<_, i64>(5)? as u64,
        date_at: row.get::<_, Option<i64>>(6)?.map(|value| value as u64),
        sort_at: row.get::<_, i64>(7)? as u64,
        snippet: row.get(8)?,
        has_attachments: row.get(9)?,
        seen: row.get(10)?,
        flagged: row.get(11)?,
        answered: row.get(12)?,
        deleted: row.get(13)?,
        folder: row.get(14)?,
        folder_role: row.get(15)?,
        thread_id: row.get(16)?,
    })
}

/// `attachments` 的元数据列。列序即 [`decode_attachment_row`] 的下标；`stored` 由
/// `content IS NOT NULL` 物化，字节本身不在这组列里。
const ATTACHMENT_COLUMNS: &str =
    "part_index, filename, mime_type, content_id, inline, size, content IS NOT NULL";

fn decode_attachment_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailAttachment> {
    Ok(MailAttachment {
        part_index: row.get(0)?,
        filename: row.get(1)?,
        mime_type: row.get(2)?,
        content_id: row.get(3)?,
        inline: row.get(4)?,
        size: row.get::<_, i64>(5)? as u64,
        stored: row.get(6)?,
        content: None,
    })
}

/// [`Store::read_attachment`] 的四种结果（ADR-0032 决策 3）。
#[derive(Debug)]
pub enum AttachmentLookup {
    /// `(account, folder, uid)` 解析不到可见消息。
    MessageMissing,
    /// 消息存在，但 `part_index` 不是它的附件。
    PartMissing,
    /// part 存在，字节未落库（超限或历史缓存）。元数据完整，`content` 为 `None`。
    NotStored(MailAttachment),
    /// 元数据与字节齐全。
    Stored(MailAttachment),
}

// —— 草稿的行编解码与附件引用（ADR-0034）——

/// `drafts` 的读取列。列序即 [`decode_draft_row`] 的下标。
const DRAFT_COLUMNS: &str = "d.id, d.account, d.from_address, d.to_addrs, d.cc_addrs, \
     d.bcc_addrs, d.subject, d.body, d.body_html, d.in_reply_to, d.reference_ids, d.version, \
     d.created_at, d.updated_at, d.send_state, d.sent_archive";

/// 一次草稿写入的结果（ADR-0034 决策 3/5/6）。
#[derive(Debug)]
pub enum DraftWrite {
    /// 已提交。`update` 与 `delete` 返回的分别是更新后与被删掉的那一份。
    /// 装箱是因为一份草稿比其余全部变体大两个数量级，而写入路径本来就已经做过一次事务提交。
    Saved(Box<Draft>),
    NotFound,
    /// `base_version` 与当前值不符，什么都没写。
    Conflict {
        version: u64,
    },
    SendLocked,
    /// 该账户的草稿份数已到顶。
    LimitReached { limit: i64 },
    /// 第 `position` 条附件引用的 `(account, folder, uid)` 解析不到可见消息。
    AttachmentMessageMissing { position: usize },
    /// 消息在，但 `part_index` 不是它的附件。
    AttachmentPartMissing { position: usize },
    /// part 在，但字节未落库——引用它的草稿发不出去，不如现在就拒绝。
    AttachmentNotStored { position: usize },
}

/// 收件人列表存成 JSON 数组：显示名形式（`"Doe, John" <j@d.example>`）里本来就有逗号，
/// 用逗号当分隔符等于自埋一个「收件人被劈成两个」的 bug（ADR-0034 决策 2）。
fn encode_addresses(values: &[String]) -> String {
    serde_json::to_string(values).expect("字符串数组可序列化")
}

fn decode_addresses(index: usize, text: &str) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("草稿收件人列不是 JSON 字符串数组: {error}"),
            )),
        )
    })
}

fn decode_draft_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Draft> {
    Ok(Draft {
        id: row.get(0)?,
        account: row.get(1)?,
        from: row.get(2)?,
        to: decode_addresses(3, &row.get::<_, String>(3)?)?,
        cc: decode_addresses(4, &row.get::<_, String>(4)?)?,
        bcc: decode_addresses(5, &row.get::<_, String>(5)?)?,
        subject: row.get(6)?,
        body: row.get(7)?,
        body_html: row.get(8)?,
        in_reply_to: row.get(9)?,
        references: row.get(10)?,
        attachments: Vec::new(),
        version: row.get::<_, i64>(11)? as u64,
        created_at: row.get::<_, i64>(12)? as u64,
        updated_at: row.get::<_, i64>(13)? as u64,
        send_state: row.get(14)?,
        sent_archive: row.get(15)?,
    })
}

fn load_draft(conn: &Connection, id: &str) -> Result<Option<Draft>, rusqlite::Error> {
    conn.query_row(
        &format!("SELECT {DRAFT_COLUMNS} FROM drafts d WHERE d.id = ?1"),
        params![id],
        decode_draft_row,
    )
    .optional()
}

/// 读回附件引用。`stored` 是**现查的**：源邮件可能在草稿写完之后被删掉，
/// 让 UI 能提前把话说在前面，而不是等到发信才失败（ADR-0034 决策 6）。
fn read_draft_attachments(
    conn: &Connection,
    draft_id: &str,
) -> Result<Vec<DraftAttachmentRef>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT account, folder, uid, part_index, filename, mime_type,
                CASE WHEN upload_id IS NOT NULL AND content IS NOT NULL THEN length(content) ELSE size END,
                upload_id, content IS NOT NULL AND typeof(content)='blob' AND length(content)=size AND size<=?2
         FROM draft_attachments WHERE draft_id = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map(params![draft_id, crate::uploads::FILE_BYTES], |row| {
        Ok(DraftAttachmentRef {
            account: row.get(0)?,
            folder: row.get(1)?,
            uid: row.get(2)?,
            part_index: row.get(3)?,
            filename: row.get(4)?,
            mime_type: row.get(5)?,
            size: row.get::<_, i64>(6)? as u64,
            stored: row.get(8)?,
            upload_id: row.get(7)?,
        })
    })?;
    let mut refs = Vec::new();
    for row in rows {
        let mut reference: DraftAttachmentRef = row?;
        if reference.upload_id.is_some() {
            reference.stored &= crate::uploads::validate_metadata(
                &reference.filename, &reference.mime_type, reference.size,
            ).is_ok();
        } else {
            reference.stored = matches!(
                lookup_attachment(
                    conn,
                    &reference.account,
                    &reference.folder,
                    &reference.uid,
                    reference.part_index,
                    false,
                )?,
                AttachmentLookup::Stored(_)
            );
        }
        refs.push(reference);
    }
    Ok(refs)
}

/// 整体替换一份草稿的附件引用。每条都要**当场**在库里查到且字节已落库，
/// 否则整次写入被拒绝——存进一条读的时候才发现不存在的引用毫无意义。
#[allow(clippy::type_complexity)]
fn write_draft_attachments(
    conn: &Connection,
    draft_id: &str,
    draft_account: &str,
    inputs: Option<&[DraftAttachmentInput]>,
) -> Result<Result<Vec<DraftAttachmentRef>, DraftWrite>, rusqlite::Error> {
    let Some(inputs) = inputs else {
        return Ok(Ok(Vec::new()));
    };
    let mut uploaded = std::collections::HashMap::new();
    for (position, input) in inputs.iter().enumerate() {
        if let Some(id) = &input.upload_id {
            if input.account.is_some()
                || !input.folder.is_empty()
                || !input.uid.is_empty()
                || input.part_index != 0
            {
                return Ok(Err(DraftWrite::AttachmentPartMissing { position }));
            }
            if let Some(row) = uploads::load_file(conn, draft_id, id)? {
                uploaded.insert(id.clone(), row);
            }
        }
    }
    conn.execute(
        "DELETE FROM draft_attachments WHERE draft_id = ?1",
        params![draft_id],
    )?;
    let mut refs = Vec::with_capacity(inputs.len());
    for (position, input) in inputs.iter().enumerate() {
        if let Some(id) = &input.upload_id {
            let Some(file) = uploaded.remove(id) else {
                return Ok(Err(DraftWrite::AttachmentPartMissing { position }));
            };
            uploads::insert_file(conn, draft_id, draft_account, position, id, &file)?;
            refs.push(file.reference(draft_account, id));
            continue;
        }
        let account = input.account.as_deref().unwrap_or(draft_account);
        let found = lookup_attachment(
            conn,
            account,
            &input.folder,
            &input.uid,
            input.part_index,
            false,
        )?;
        let attachment = match found {
            AttachmentLookup::Stored(attachment) => attachment,
            AttachmentLookup::MessageMissing => {
                return Ok(Err(DraftWrite::AttachmentMessageMissing { position }))
            }
            AttachmentLookup::PartMissing => {
                return Ok(Err(DraftWrite::AttachmentPartMissing { position }))
            }
            AttachmentLookup::NotStored(_) => {
                return Ok(Err(DraftWrite::AttachmentNotStored { position }))
            }
        };
        conn.execute(
            "INSERT INTO draft_attachments
                 (draft_id, position, account, folder, uid, part_index, filename, mime_type, size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                draft_id,
                position as i64,
                account,
                input.folder,
                input.uid,
                input.part_index,
                attachment.filename,
                attachment.mime_type,
                attachment.size as i64,
            ],
        )?;
        refs.push(DraftAttachmentRef {
            upload_id: None,
            account: account.to_owned(),
            folder: input.folder.clone(),
            uid: input.uid.clone(),
            part_index: input.part_index,
            filename: attachment.filename,
            mime_type: attachment.mime_type,
            size: attachment.size,
            stored: true,
        });
    }
    Ok(Ok(refs))
}

/// 组装 `WHERE`，并给出按顺序绑定的参数。空过滤返回空子句。
fn message_filter(account: Option<&str>, folder: Option<&str>) -> (String, Vec<String>) {
    let mut clauses: Vec<String> = vec![VISIBLE.to_string()];
    let mut params: Vec<String> = Vec::new();
    if let Some(account) = account {
        params.push(account.to_string());
        clauses.push(format!("m.account = ?{}", params.len()));
    }
    if let Some(folder) = folder {
        params.push(folder.to_string());
        clauses.push(format!("f.name = ?{}", params.len()));
    }
    (format!("WHERE {}", clauses.join(" AND ")), params)
}

fn insert_op(
    conn: &Connection,
    account: &str,
    folder_id: i64,
    uid: &str,
    kind: OpKind,
    payload: &str,
    now: u64,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO pending_ops
           (account, folder_id, uid, kind, payload, state, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?6)",
        params![account, folder_id, uid, kind.as_str(), payload, now as i64],
    )?;
    Ok(())
}

fn decode_pending_op_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingOp> {
    let kind_value = row.get::<_, String>(5)?;
    let kind = OpKind::parse(&kind_value).ok_or_else(|| {
        invalid_address_column(5, Type::Text, format!("未知 op kind: {kind_value}"))
    })?;
    let state_value = row.get::<_, String>(7)?;
    let state = OpState::parse(&state_value).ok_or_else(|| {
        invalid_address_column(7, Type::Text, format!("未知 op state: {state_value}"))
    })?;
    Ok(PendingOp {
        id: row.get(0)?,
        account: row.get(1)?,
        folder_id: row.get(2)?,
        folder: row.get(3)?,
        uid: row.get(4)?,
        kind,
        payload: row.get(6)?,
        state,
        attempts: row.get(8)?,
        created_at: row.get::<_, i64>(9)? as u64,
        updated_at: row.get::<_, i64>(10)? as u64,
        detail: row.get(11)?,
    })
}

const PENDING_OP_COLUMNS: &str = "o.id, o.account, o.folder_id, COALESCE(f.name, ''), o.uid, \
     o.kind, o.payload, o.state, o.attempts, o.created_at, o.updated_at, o.detail";

const PENDING_OP_FROM: &str = "FROM pending_ops o LEFT JOIN folders f ON f.id = o.folder_id";

/// 空 `folder` 视为账户的 `INBOX`：v4 之前只有一个信箱，委托 provider 也只有一个
/// （ADR-0026 决策 2）。
fn normalized_folder(folder: &str) -> &str {
    if folder.is_empty() {
        "INBOX"
    } else {
        folder
    }
}

/// 按四元组解析一个 part（ADR-0032 决策 2/3）。`with_bytes = false` 时只取元数据——
/// 草稿的附件引用只需要知道「还在不在」，不需要把字节读进内存（ADR-0034 决策 6）。
///
/// `Store::read_attachment` 与草稿写入共用这一段：两处对「拿不到」的判定必须一致，
/// 否则草稿里会存进一条读的时候才发现不存在的引用。
fn lookup_attachment(
    conn: &Connection,
    account: &str,
    folder: &str,
    uid: &str,
    part_index: u32,
    with_bytes: bool,
) -> Result<AttachmentLookup, rusqlite::Error> {
    let Some(folder_id) = lookup_folder(conn, account, folder)? else {
        return Ok(AttachmentLookup::MessageMissing);
    };
    let visible: Option<i64> = conn
        .query_row(
            &format!(
                "SELECT m.id {MESSAGE_FROM}
                 WHERE {VISIBLE} AND m.account = ?1 AND m.folder_id = ?2 AND m.uid = ?3"
            ),
            params![account, folder_id, uid],
            |row| row.get(0),
        )
        .optional()?;
    if visible.is_none() {
        return Ok(AttachmentLookup::MessageMissing);
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {ATTACHMENT_COLUMNS}, content FROM attachments
         WHERE account = ?1 AND folder_id = ?2 AND uid = ?3 AND part_index = ?4"
    ))?;
    let mut rows = stmt.query(params![account, folder_id, uid, part_index])?;
    let Some(row) = rows.next()? else {
        return Ok(AttachmentLookup::PartMissing);
    };
    let mut attachment = decode_attachment_row(row)?;
    if !attachment.stored {
        return Ok(AttachmentLookup::NotStored(attachment));
    }
    if with_bytes {
        attachment.content = row.get(7)?;
    }
    Ok(AttachmentLookup::Stored(attachment))
}

fn lookup_folder(
    conn: &Connection,
    account: &str,
    folder: &str,
) -> Result<Option<i64>, rusqlite::Error> {
    conn.query_row(
        "SELECT id FROM folders WHERE account = ?1 AND name = ?2",
        params![account, normalized_folder(folder)],
        |row| row.get::<_, i64>(0),
    )
    .optional()
}

/// 取回或创建文件夹行。落库路径不能因为「LIST 还没跑过」就丢消息；
/// 隐式创建时 role 只按名字判定，`LIST` 之后会被 `upsert_folder` 用服务器的明示声明纠正。
fn resolve_folder(
    conn: &Connection,
    account: &str,
    folder: &str,
) -> Result<i64, rusqlite::Error> {
    let name = normalized_folder(folder);
    if let Some(id) = lookup_folder(conn, account, name)? {
        return Ok(id);
    }
    let display_name = crate::folders::decode_modified_utf7(name);
    let role = crate::folders::resolve_role(&[], name, &display_name, None);
    conn.execute(
        "INSERT OR IGNORE INTO folders (account, name, role, display_name)
         VALUES (?1, ?2, ?3, ?4)",
        params![account, name, FolderRole::to_column(role), display_name],
    )?;
    lookup_folder(conn, account, name)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// 把一封消息写进 FTS5 索引（ADR-0029 决策 2、3）。
///
/// 先删后插，因此重复调用幂等——回填与实时落库都可能落到同一行上。
/// 写进去的不是原文而是**分析结果**：表意文字已逐字切开，任何路径都不会把它当正文读出来。
fn index_message(conn: &Connection, row_id: i64, msg: &MailMessage) -> Result<(), rusqlite::Error> {
    let recipients = if msg.cc.is_empty() {
        msg.recipient.clone()
    } else {
        format!("{} {}", msg.recipient, msg.cc)
    };
    conn.execute("DELETE FROM messages_fts WHERE rowid = ?1", params![row_id])?;
    conn.execute(
        "INSERT INTO messages_fts (rowid, subject, sender, recipients, body)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            row_id,
            segment(&msg.subject),
            segment(&msg.sender),
            segment(&recipients),
            segment(&msg.body),
        ],
    )?;
    Ok(())
}

/// 把一封消息并进线程图，返回它的 `thread_id`（ADR-0029 决策 7）。
///
/// 边包含**尚未见过的** id，所以到达顺序无关：先到的回复留下指向未来父信的边，
/// 父信后到时用自己的 `Message-ID` 就能查到它。回填与实时落库因此可以并发进行。
fn assign_thread(
    conn: &Connection,
    account: &str,
    row_id: i64,
    msg: &MailMessage,
) -> Result<i64, rusqlite::Error> {
    let refs = thread_ref_ids(&msg.message_id, &msg.in_reply_to, &msg.references);
    let mut threads: Vec<i64> = Vec::new();
    if !refs.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT m.thread_id FROM thread_refs r
             JOIN messages m ON m.id = r.message_row_id
             WHERE r.account = ?1 AND r.ref_id = ?2 AND m.thread_id != 0 AND m.id != ?3",
        )?;
        for id in &refs {
            let rows = stmt.query_map(params![account, id, row_id], |row| row.get::<_, i64>(0))?;
            for thread in rows {
                let thread = thread?;
                if !threads.contains(&thread) {
                    threads.push(thread);
                }
            }
        }
    }
    // 没有任何关联 → 用自己的行 id 开一条新线程（天然唯一，不需要额外计数器）。
    let thread_id = threads.iter().copied().min().unwrap_or(row_id);
    for other in threads.iter().copied().filter(|value| *value != thread_id) {
        conn.execute(
            "UPDATE messages SET thread_id = ?1 WHERE account = ?2 AND thread_id = ?3",
            params![thread_id, account, other],
        )?;
    }
    conn.execute(
        "UPDATE messages SET thread_id = ?1 WHERE id = ?2",
        params![thread_id, row_id],
    )?;
    for id in &refs {
        conn.execute(
            "INSERT OR IGNORE INTO thread_refs (account, ref_id, message_row_id)
             VALUES (?1, ?2, ?3)",
            params![account, id, row_id],
        )?;
    }
    Ok(thread_id)
}

/// 一次本地检索的全部维度（ADR-0029 决策 4、5）。
#[derive(Clone, Debug, Default)]
pub struct SearchQuery {
    /// 用户原文。内核自己切词并构造 `MATCH`，绝不透传给 FTS5。
    pub text: String,
    pub account: Option<String>,
    pub folder: Option<String>,
    /// 发件人子串（大小写不敏感）。
    pub sender: Option<String>,
    /// 收件人（`recipient` 或 `cc`）子串（大小写不敏感）。
    pub recipient: Option<String>,
    /// 主题子串（大小写不敏感）。
    pub subject: Option<String>,
    /// 时间范围按 `sort_at` —— 与列表显示的时间是同一个。
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub has_attachments: Option<bool>,
    pub seen: Option<bool>,
    pub flagged: Option<bool>,
    pub limit: usize,
    pub offset: usize,
}

/// 检索索引的进度（ADR-0029 决策 9）。回填期间结果是部分的，这是事实，所以它可查询。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexProgress {
    /// 还没进索引的历史消息条数。
    pub pending: i64,
    pub done: bool,
}

/// 组装检索的 `JOIN` / `WHERE` / 绑定参数。
fn search_clauses(query: &SearchQuery) -> (&'static str, String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let mut params: Vec<Value> = Vec::new();
    let mut clauses: Vec<String> = vec![VISIBLE.to_string()];
    let mut join = "";
    if let Some(expression) = build_match_expression(&query.text) {
        params.push(Value::Text(expression));
        join = " JOIN messages_fts ON messages_fts.rowid = m.id";
        clauses.push(format!("messages_fts MATCH ?{}", params.len()));
    }
    if let Some(account) = &query.account {
        params.push(Value::Text(account.clone()));
        clauses.push(format!("m.account = ?{}", params.len()));
    }
    if let Some(folder) = &query.folder {
        params.push(Value::Text(normalized_folder(folder).to_string()));
        clauses.push(format!("f.name = ?{}", params.len()));
    }
    if let Some(sender) = query.sender.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        params.push(Value::Text(sender.to_string()));
        clauses.push(format!("instr(lower(m.sender), lower(?{})) > 0", params.len()));
    }
    if let Some(recipient) = query
        .recipient
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        params.push(Value::Text(recipient.to_string()));
        let idx = params.len();
        clauses.push(format!(
            "((instr(lower(m.recipient), lower(?{idx})) > 0) OR (instr(lower(m.cc), lower(?{idx})) > 0))"
        ));
    }
    if let Some(subject) = query
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        params.push(Value::Text(subject.to_string()));
        clauses.push(format!("instr(lower(m.subject), lower(?{})) > 0", params.len()));
    }
    if let Some(since) = query.since {
        params.push(Value::Integer(i64::try_from(since).unwrap_or(i64::MAX)));
        clauses.push(format!("m.sort_at >= ?{}", params.len()));
    }
    if let Some(until) = query.until {
        params.push(Value::Integer(i64::try_from(until).unwrap_or(i64::MAX)));
        clauses.push(format!("m.sort_at <= ?{}", params.len()));
    }
    if let Some(has_attachments) = query.has_attachments {
        params.push(Value::Integer(i64::from(has_attachments)));
        clauses.push(format!("m.has_attachments = ?{}", params.len()));
    }
    if let Some(seen) = query.seen {
        params.push(Value::Integer(i64::from(seen)));
        clauses.push(format!("m.seen = ?{}", params.len()));
    }
    if let Some(flagged) = query.flagged {
        params.push(Value::Integer(i64::from(flagged)));
        clauses.push(format!("m.flagged = ?{}", params.len()));
    }
    (join, format!("WHERE {}", clauses.join(" AND ")), params)
}

fn read_index_state(conn: &Connection) -> Result<(i64, bool), rusqlite::Error> {
    conn.query_row(
        "SELECT cursor, done FROM index_state WHERE name = 'search'",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
    )
    .optional()
    // 行缺失按「无待办」处理：索引不完整只影响搜索结果，不该挡住整库读取。
    .map(|value| value.unwrap_or((0, true)))
}

fn read_index_progress(conn: &Connection) -> Result<IndexProgress, rusqlite::Error> {
    let (cursor, done) = read_index_state(conn)?;
    if done {
        return Ok(IndexProgress { pending: 0, done: true });
    }
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE id <= ?1",
        params![cursor],
        |row| row.get(0),
    )?;
    Ok(IndexProgress { pending, done: false })
}

fn decode_folder_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FolderRecord> {
    let role_value = row.get::<_, String>(4)?;
    Ok(FolderRecord {
        id: row.get(0)?,
        account: row.get(1)?,
        name: row.get(2)?,
        display_name: row.get(3)?,
        // 未知 role 字符串按「无角色」处理而不是报错：role 只影响图标与同步归类，
        // 让一个新版本写入的角色名挡住整库读取不值得。
        role: FolderRole::parse(&role_value),
        uid_validity: row.get::<_, Option<i64>>(5)?.map(|value| value as u32),
        last_seen_uid: row.get::<_, Option<i64>>(6)?.map(|value| value as u32),
        synced_at: row.get::<_, Option<i64>>(7)?.map(|value| value as u64),
        present: row.get(8)?,
    })
}

const FOLDER_COLUMNS: &str =
    "id, account, name, display_name, role, uid_validity, last_seen_uid, synced_at, present";

impl Service for Store {
    fn name(&self) -> &'static str {
        "store"
    }
}

impl Store {
    pub fn open(path: &str) -> Result<Store, StoreOpenError> {
        let (mut conn, database_lock, db_path) = if path.is_empty() {
            (Connection::open_in_memory()?, None, String::new())
        } else {
            let lock = DatabaseLock::acquire(std::path::Path::new(path))?;
            #[cfg(unix)]
            lock::secure_unix_database_file(lock.database_path())?;
            let db_path = lock.database_path().to_string_lossy().into_owned();
            let connection = Connection::open(lock.database_path())?;
            (connection, Some(lock), db_path)
        };
        migrations::prepare(&mut conn)?;
        if database_lock.is_some() {
            // 内存库的页就是数据本身，不能收紧页缓存；只对文件库生效。
            migrations::apply_low_footprint_pragmas(&conn);
        }
        conn.execute(
            "UPDATE drafts SET send_state='unknown', version=version+1 WHERE send_state='sending'",
            [],
        )?;
        Ok(Store {
            conn: Mutex::new(conn),
            uploads: Arc::new(Mutex::new(crate::uploads::Uploads::default())),
            _database_lock: database_lock,
            db_path,
        })
    }

    /// 归还 SQLite 可释放的内存，并做一次不阻塞读写的 WAL checkpoint（ADR-0041 决策 5）。
    ///
    /// 在大批写入（冷启动同步、检索索引回填）结束后调用。尽力而为，失败静默。
    pub fn shrink_memory(&self) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute_batch("PRAGMA shrink_memory;");
        // checkpoint 会回一行（busy, log, checkpointed）：用 query_row 读完。
        let _ = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
            row.get::<_, i64>(0)
        });
    }

    /// 当前数据库的磁盘与内容占用快照。读失败的分量按 0 计（观测接口不该报错）。
    pub fn storage_metrics(&self) -> StoreMetrics {
        let file_len = |path: &str| std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        let conn = self.conn.lock().unwrap();
        let count = |sql: &str| -> u64 {
            conn.query_row(sql, [], |row| row.get::<_, i64>(0))
                .map(|value| value.max(0) as u64)
                .unwrap_or(0)
        };
        let (db_bytes, wal_bytes) = if self.db_path.is_empty() {
            let pages = count("PRAGMA page_count");
            let page_size = count("PRAGMA page_size");
            (pages.saturating_mul(page_size), 0)
        } else {
            (
                file_len(&self.db_path),
                file_len(&format!("{}-wal", self.db_path)),
            )
        };
        StoreMetrics {
            db_bytes,
            wal_bytes,
            message_count: count("SELECT COUNT(*) FROM messages"),
            stored_attachment_bytes: count(
                "SELECT COALESCE(SUM(LENGTH(content)), 0) FROM attachments WHERE content IS NOT NULL",
            ),
        }
    }

    /// 落库一条消息及其附件；重复 (account, uid) 幂等返回 `Ok(false)`。
    ///
    /// `sort_at` 在此**重新计算**而不是照抄载荷：排序键的可信度判定只有一处来源，
    /// 任何构造路径（IMAP、provider、回填）都不可能把它写错（ADR-0025 决策 3）。
    /// 消息行与附件行在同一 IMMEDIATE 事务内提交。
    pub fn insert_message(&self, msg: &MailMessage) -> Result<bool, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let folder_id = resolve_folder(&tx, &msg.account, &msg.folder)?;
        let sort_at = mime::sort_key(msg.date_at, msg.received_at);
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO messages
               (uid, account, sender, recipient, subject, body, received_at,
                message_id, in_reply_to, reference_ids, cc, bcc, reply_to,
                date_at, sort_at, body_html, snippet, has_attachments,
                seen, flagged, answered, deleted, folder_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
            params![
                msg.uid,
                msg.account,
                msg.sender,
                msg.recipient,
                msg.subject,
                msg.body,
                msg.received_at as i64,
                msg.message_id,
                msg.in_reply_to,
                msg.references,
                msg.cc,
                msg.bcc,
                msg.reply_to,
                msg.date_at.map(|value| value as i64),
                sort_at as i64,
                msg.body_html,
                msg.snippet,
                msg.has_attachments,
                msg.seen,
                msg.flagged,
                msg.answered,
                msg.deleted,
                folder_id,
            ],
        )?;
        if inserted > 0 {
            let row_id = tx.last_insert_rowid();
            index_message(&tx, row_id, msg)?;
            assign_thread(&tx, &msg.account, row_id, msg)?;
            for attachment in &msg.attachments {
                tx.execute(
                    "INSERT OR IGNORE INTO attachments
                       (account, folder_id, uid, part_index, filename, mime_type, content_id,
                        inline, size, content)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        msg.account,
                        folder_id,
                        msg.uid,
                        attachment.part_index,
                        attachment.filename,
                        attachment.mime_type,
                        attachment.content_id,
                        attachment.inline,
                        attachment.size as i64,
                        attachment.content,
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(inserted > 0)
    }

    /// 一条消息的附件元数据（不含字节），part 序。
    /// `folder` 是服务器线上原名，空串表示账户的 `INBOX`。
    pub fn list_attachments(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
    ) -> Result<Vec<MailAttachment>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let Some(folder_id) = lookup_folder(&conn, account, folder)? else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT {ATTACHMENT_COLUMNS}
             FROM attachments WHERE account = ?1 AND folder_id = ?2 AND uid = ?3
             ORDER BY part_index"
        ))?;
        let rows = stmt.query_map(params![account, folder_id, uid], decode_attachment_row)?;
        rows.collect()
    }

    /// 按完整四元组读取一个附件 part 的元数据与字节（ADR-0032 决策 2/3）。
    ///
    /// `folder` **必填**（空串仍按 `INBOX` 归一，但调用方——API 层——不允许省略）：
    /// UID 只在 mailbox 内唯一，字节读取不给 `get_message` 那种消歧便利。消息必须可见
    /// （`pending_removal = 0`）。三种「拿不到」各自有名字，API 层据此分配错误码。
    pub fn read_attachment(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
        part_index: u32,
    ) -> Result<AttachmentLookup, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        lookup_attachment(&conn, account, folder, uid, part_index, true)
    }

    /// 按 `Content-ID` 取回内联附件（含字节）。正文渲染的 `cid:` 解析用（M4.5-04）。
    /// `content` 为 `None` 表示该 part 超过内联上限，只有元数据。
    pub fn attachment_by_content_id(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
        content_id: &str,
    ) -> Result<Option<MailAttachment>, rusqlite::Error> {
        if content_id.is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().unwrap();
        let Some(folder_id) = lookup_folder(&conn, account, folder)? else {
            return Ok(None);
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT {ATTACHMENT_COLUMNS}, content
             FROM attachments WHERE account = ?1 AND folder_id = ?2 AND uid = ?3
               AND content_id = ?4
             ORDER BY part_index LIMIT 1"
        ))?;
        let mut rows = stmt.query(params![account, folder_id, uid, content_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let mut attachment = decode_attachment_row(row)?;
        attachment.content = row.get(7)?;
        Ok(Some(attachment))
    }

    /// 按 part 序号取回附件字节。
    pub fn attachment_content(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
        part_index: u32,
    ) -> Result<Option<Vec<u8>>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let Some(folder_id) = lookup_folder(&conn, account, folder)? else {
            return Ok(None);
        };
        conn.query_row(
            "SELECT content FROM attachments
             WHERE account = ?1 AND folder_id = ?2 AND uid = ?3 AND part_index = ?4",
            params![account, folder_id, uid, part_index],
            |row| row.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()
        .map(Option::flatten)
    }

    pub fn count(&self) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
    }

    /// 指定账户的最大数字 uid（委托收信游标，ADR-0021 §4）。无消息返回 None。
    /// 非数字 uid 不参与比较（内置 IMAP 账户不产生此类游标）。
    pub fn max_numeric_uid(&self, account: &str) -> Result<Option<u64>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT MAX(CAST(uid AS INTEGER)) FROM messages
             WHERE account = ?1 AND uid GLOB '[0-9]*'",
            params![account],
            |row| row.get::<_, Option<u64>>(0),
        )
    }

    /// 按账户统计消息；`None` 表示所有账户。
    pub fn count_messages(
        &self,
        account: Option<&str>,
        folder: Option<&str>,
    ) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let (filter, filter_params) = message_filter(account, folder);
        let bound: Vec<&dyn rusqlite::ToSql> =
            filter_params.iter().map(|value| value as &dyn rusqlite::ToSql).collect();
        conn.query_row(
            &format!("SELECT COUNT(*) {MESSAGE_FROM} {filter}"),
            bound.as_slice(),
            |row| row.get(0),
        )
    }

    /// 分页列出轻量消息摘要（最近在前），可按账户过滤。
    /// `folder` 是服务器线上原名；`None` = 不按文件夹过滤（旧客户端行为逐字节不变）。
    pub fn list_message_summaries(
        &self,
        account: Option<&str>,
        folder: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MessageSummary>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let (filter, filter_params) = message_filter(account, folder);
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUMMARY_COLUMNS} {MESSAGE_FROM} {filter} {MESSAGE_ORDER}
             LIMIT ?{} OFFSET ?{}",
            filter_params.len() + 1,
            filter_params.len() + 2,
        ))?;
        let limit = limit as i64;
        let offset = offset as i64;
        let mut bound: Vec<&dyn rusqlite::ToSql> =
            filter_params.iter().map(|value| value as &dyn rusqlite::ToSql).collect();
        bound.push(&limit);
        bound.push(&offset);
        let rows = stmt.query_map(bound.as_slice(), decode_summary_row)?;
        rows.collect()
    }

    /// 分页列出消息（最近在前）。api 插件的 `listMessages` 用。
    ///
    /// **不返回 HTML 原文与附件**：该方法一次可返回 500 条完整消息，带上 HTML
    /// 会越过协议单帧上限（ADR-0025 决策 6）。HTML 正文只经 `getMessage` 取回。
    pub fn list_messages(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<MailMessage>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} {MESSAGE_FROM} WHERE {VISIBLE} {MESSAGE_ORDER}
             LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], decode_message_row)?;
        rows.collect()
    }

    /// 按账户、文件夹和 UID 查询完整持久化消息，含 HTML 原文与附件元数据。
    /// 附件字节不随消息返回（见 [`Store::attachment_by_content_id`]）。
    ///
    /// `folder` 为 `None` 时**不唯一**——UID 只在 mailbox 内唯一（ADR-0026 决策 1）。
    /// 此时的消歧规则是固定的：收件箱优先，其次落库序最早的一条。客户端应当带上
    /// 摘要里返回的 `folder`。
    pub fn get_message(
        &self,
        account: &str,
        folder: Option<&str>,
        uid: &str,
    ) -> Result<Option<MailMessage>, rusqlite::Error> {
        let message = {
            let conn = self.conn.lock().unwrap();
            let (folder_clause, folder_param) = match folder {
                Some(folder) => (" AND f.name = ?3", Some(normalized_folder(folder))),
                None => ("", None),
            };
            let mut stmt = conn.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS}, m.body_html {MESSAGE_FROM}
                 WHERE {VISIBLE} AND m.account = ?1 AND m.uid = ?2{folder_clause}
                 ORDER BY (f.role = 'inbox') DESC, m.id ASC LIMIT 1"
            ))?;
            let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&account, &uid];
            if let Some(folder) = folder_param.as_ref() {
                bound.push(folder);
            }
            let mut rows = stmt.query(bound.as_slice())?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            let mut message = decode_message_row(row)?;
            message.body_html = row.get(24)?;
            message
        };
        let attachments = self.list_attachments(account, &message.folder, uid)?;
        Ok(Some(MailMessage {
            attachments,
            ..message
        }))
    }

    // —— 本地检索与线程（ADR-0029） ——

    /// 全文检索 + 结构化过滤，返回一页摘要与命中总数。
    ///
    /// 排序与列表完全一致（`sort_at DESC, id DESC`），不按相关度——邮箱搜索的用户模型是
    /// 「我知道有这么一封」，最强的信号是时间；bm25 还会让同一次搜索在不同时间给出不同
    /// 顺序（ADR-0029 决策 6）。
    pub fn search_messages(
        &self,
        query: &SearchQuery,
    ) -> Result<(Vec<MessageSummary>, i64), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let (join, filter, params) = search_clauses(query);
        let bound: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|value| value as &dyn rusqlite::ToSql).collect();
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) {MESSAGE_FROM}{join} {filter}"),
            bound.as_slice(),
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUMMARY_COLUMNS} {MESSAGE_FROM}{join} {filter} {MESSAGE_ORDER}
             LIMIT ?{} OFFSET ?{}",
            params.len() + 1,
            params.len() + 2,
        ))?;
        let limit = query.limit as i64;
        let offset = query.offset as i64;
        let mut bound: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|value| value as &dyn rusqlite::ToSql).collect();
        bound.push(&limit);
        bound.push(&offset);
        let rows = stmt.query_map(bound.as_slice(), decode_summary_row)?;
        Ok((rows.collect::<Result<Vec<_>, _>>()?, total))
    }

    /// 联系人检索阶段 1（锁内只取原始头行）：从指定账户拉取有界近期样本与近期已发送样本。
    ///
    /// ADR-0034：绝不查询 `drafts` 表；仅读取存在且非 spam/trash/drafts 文件夹下的可见未删消息
    /// （`pending_removal = 0 AND deleted = 0`），严格绑定单个 `account`。
    pub fn recent_contact_rows(
        &self,
        account: &str,
        recent_limit: usize,
        sent_limit: usize,
    ) -> Result<Vec<crate::contacts::ContactMessageRow>, rusqlite::Error> {
        let decode_contact_row = |row: &rusqlite::Row<'_>| {
            Ok(crate::contacts::ContactMessageRow {
                id: row.get(0)?,
                sender: row.get(1)?,
                recipient: row.get(2)?,
                cc: row.get(3)?,
                sort_at: row.get::<_, i64>(4)?.max(0) as u64,
                folder_role: row.get(5)?,
            })
        };

        let conn = self.conn.lock().unwrap();
        let mut out = Vec::new();
        if recent_limit > 0 {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.sender, m.recipient, m.cc, m.sort_at, f.role
                 FROM messages m
                 JOIN folders f ON f.id = m.folder_id AND f.account = m.account
                 WHERE m.account = ?1
                   AND m.pending_removal = 0
                   AND m.deleted = 0
                   AND f.role NOT IN ('spam', 'trash', 'drafts')
                 ORDER BY m.sort_at DESC, m.id DESC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![account, recent_limit as i64], decode_contact_row)?;
            for row in rows {
                out.push(row?);
            }
        }

        if sent_limit > 0 {
            let sent_folder_ids: Vec<i64> = {
                let mut stmt = conn.prepare(
                    "SELECT id FROM folders WHERE account = ?1 AND role = 'sent'",
                )?;
                let rows = stmt.query_map(params![account], |row| row.get::<_, i64>(0))?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            if !sent_folder_ids.is_empty() {
                let mut stmt = conn.prepare(
                    "SELECT m.id, m.sender, m.recipient, m.cc, m.sort_at, 'sent'
                     FROM messages m
                     WHERE m.folder_id = ?1
                       AND m.account = ?2
                       AND m.pending_removal = 0
                       AND m.deleted = 0
                     ORDER BY m.sort_at DESC, m.id DESC
                     LIMIT ?3",
                )?;
                for folder_id in sent_folder_ids {
                    let rows = stmt.query_map(
                        params![folder_id, account, sent_limit as i64],
                        decode_contact_row,
                    )?;
                    for row in rows {
                        out.push(row?);
                    }
                }
            }
        }

        Ok(out)
    }

    /// 联系人检索阶段 2（锁内只取原始头行）：仅在近期样本不足时执行有界转义 FTS5 前缀回退。
    ///
    /// `fts_match` 必须已限定 `{sender recipients}` 列并经转义，且 SQL 层再次强约束账户、可见性与文件夹角色。
    pub fn prefix_contact_rows(
        &self,
        account: &str,
        fts_match: &str,
        limit: usize,
    ) -> Result<Vec<crate::contacts::ContactMessageRow>, rusqlite::Error> {
        if limit == 0 || fts_match.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.sender, m.recipient, m.cc, m.sort_at, f.role
             FROM messages_fts
             JOIN messages m ON m.id = messages_fts.rowid
             JOIN folders f ON f.id = m.folder_id AND f.account = m.account
             WHERE messages_fts MATCH ?1
               AND m.account = ?2
               AND m.pending_removal = 0
               AND m.deleted = 0
               AND f.role NOT IN ('spam', 'trash', 'drafts')
             ORDER BY m.sort_at DESC, m.id DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![fts_match, account, limit as i64], |row| {
            Ok(crate::contacts::ContactMessageRow {
                id: row.get(0)?,
                sender: row.get(1)?,
                recipient: row.get(2)?,
                cc: row.get(3)?,
                sort_at: row.get::<_, i64>(4)?.max(0) as u64,
                folder_role: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// 按发件账户检索联系人建议：SQLite 锁仅覆盖有界原始行读取，所有 `mailparse` 解析、
    /// `validate_send_field` 校验、去重与加权排名均在锁外完成。
    pub fn search_contacts(
        &self,
        account: &str,
        query: &str,
        own_addresses: &[String],
        limit: usize,
    ) -> Result<Vec<crate::contacts::ContactSuggestion>, rusqlite::Error> {
        let limit = limit.clamp(1, crate::contacts::MAX_CONTACT_LIMIT);
        let mut rows = self.recent_contact_rows(
            account,
            crate::contacts::RECENT_CONTACT_SAMPLE_LIMIT,
            crate::contacts::SENT_CONTACT_SAMPLE_LIMIT,
        )?;
        let first_pass = crate::contacts::rank_contacts(&rows, query, own_addresses, limit, None);
        if !query.trim().is_empty() && first_pass.len() < limit {
            if let Some(fts_expr) = crate::contacts::build_contact_fts_prefix_expression(query) {
                let extra = self.prefix_contact_rows(
                    account,
                    &fts_expr,
                    crate::contacts::FALLBACK_PREFIX_ROW_LIMIT,
                )?;
                if !extra.is_empty() {
                    rows.extend(extra);
                    return Ok(crate::contacts::rank_contacts(
                        &rows,
                        query,
                        own_addresses,
                        limit,
                        None,
                    ));
                }
            }
        }
        Ok(first_pass)
    }

    /// 一条线程的全部消息，**时间正序**——线程是要从头读到尾的，不是列表。
    /// 跨文件夹：收件箱里的来信与已发送里的回复本来就属于同一条会话。
    pub fn thread_summaries(
        &self,
        account: &str,
        thread_id: i64,
    ) -> Result<Vec<MessageSummary>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUMMARY_COLUMNS} {MESSAGE_FROM}
             WHERE {VISIBLE} AND m.account = ?1 AND m.thread_id = ?2
             ORDER BY m.sort_at ASC, m.id ASC"
        ))?;
        let rows = stmt.query_map(params![account, thread_id], decode_summary_row)?;
        rows.collect()
    }

    /// 某封消息所属线程的 id。`folder` 为 `None` 时沿用 `get_message` 的固定消歧规则。
    pub fn thread_of(
        &self,
        account: &str,
        folder: Option<&str>,
        uid: &str,
    ) -> Result<Option<i64>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let (folder_clause, folder_param) = match folder {
            Some(folder) => (" AND f.name = ?3", Some(normalized_folder(folder))),
            None => ("", None),
        };
        let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&account, &uid];
        if let Some(folder) = folder_param.as_ref() {
            bound.push(folder);
        }
        conn.query_row(
            &format!(
                "SELECT m.thread_id {MESSAGE_FROM}
                 WHERE {VISIBLE} AND m.account = ?1 AND m.uid = ?2{folder_clause}
                 ORDER BY (f.role = 'inbox') DESC, m.id ASC LIMIT 1"
            ),
            bound.as_slice(),
            |row| row.get::<_, i64>(0),
        )
        .optional()
    }

    /// 索引回填的当前进度。
    pub fn search_index_status(&self) -> Result<IndexProgress, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        read_index_progress(&conn)
    }

    /// 推进一批索引回填（ADR-0029 决策 9）。
    ///
    /// **游标与索引写入在同一个事务里提交**，所以崩溃后从上次提交处续跑，不重复也不遗漏。
    /// 游标从 `MAX(id)` 向下走：最近的邮件最先可搜。
    pub fn backfill_search_index(&self, batch: usize) -> Result<IndexProgress, rusqlite::Error> {
        let batch = batch.max(1);
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (cursor, done) = read_index_state(&tx)?;
        if done {
            tx.commit()?;
            return Ok(IndexProgress { pending: 0, done: true });
        }
        let rows: Vec<(i64, MailMessage)> = {
            let mut stmt = tx.prepare(
                "SELECT id, account, subject, sender, recipient, cc, body,
                        message_id, in_reply_to, reference_ids
                 FROM messages WHERE id <= ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let mapped = stmt.query_map(params![cursor, batch as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    MailMessage {
                        account: row.get(1)?,
                        subject: row.get(2)?,
                        sender: row.get(3)?,
                        recipient: row.get(4)?,
                        cc: row.get(5)?,
                        body: row.get(6)?,
                        message_id: row.get(7)?,
                        in_reply_to: row.get(8)?,
                        references: row.get(9)?,
                        ..MailMessage::default()
                    },
                ))
            })?;
            mapped.collect::<Result<Vec<_>, _>>()?
        };
        let mut lowest = cursor;
        for (row_id, message) in &rows {
            index_message(&tx, *row_id, message)?;
            assign_thread(&tx, &message.account, *row_id, message)?;
            lowest = *row_id;
        }
        // 取不满一批 = 下面已经没有行了，直接收尾；否则继续从这批最小 id 之下走。
        let next_cursor = if rows.len() < batch { 0 } else { lowest - 1 };
        let finished = next_cursor <= 0;
        tx.execute(
            "UPDATE index_state SET cursor = ?1, done = ?2 WHERE name = 'search'",
            params![next_cursor.max(0), finished],
        )?;
        let progress = read_index_progress(&tx)?;
        tx.commit()?;
        Ok(progress)
    }

    /// 丢弃并重建整个检索索引与线程图。回填游标重置到 `MAX(id)`，由后台任务重新推进。
    pub fn rebuild_search_index(&self) -> Result<(), rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM messages_fts", [])?;
        tx.execute("DELETE FROM thread_refs", [])?;
        tx.execute("UPDATE messages SET thread_id = 0", [])?;
        tx.execute(
            "INSERT OR REPLACE INTO index_state (name, cursor, done)
               SELECT 'search', COALESCE(MAX(id), 0),
                      CASE WHEN MAX(id) IS NULL THEN 1 ELSE 0 END
               FROM messages",
            [],
        )?;
        tx.commit()
    }

    /// 最后落库的一条消息（插入序，非排序序；测试断言用）。
    pub fn latest(&self) -> Result<Option<MailMessage>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} {MESSAGE_FROM} WHERE {VISIBLE} ORDER BY m.id DESC LIMIT 1"
        ))?;
        let mut rows = stmt.query([])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(decode_message_row(row)?))
    }

    /// 该 (account, uid) 是否已发过 otp 通知（持久化去重，重启有效）。
    pub fn is_notified(&self, account: &str, uid: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM otp_notifications WHERE account = ?1 AND uid = ?2",
            params![account, uid],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// 标记 (account, uid) 已发过 otp 通知；重复标记幂等返回 `Ok(false)`。
    pub fn mark_notified(&self, account: &str, uid: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "INSERT OR IGNORE INTO otp_notifications (account, uid, notified_at)
             VALUES (?1, ?2, unixepoch())",
            params![account, uid],
        )?;
        Ok(n > 0)
    }

    // —— 文件夹（ADR-0026） ——

    /// 登记或更新一个文件夹，并标记它在最近一次 `LIST` 中出现过。
    /// 游标（`uid_validity` / `last_seen_uid`）不在此处改动。
    pub fn upsert_folder(
        &self,
        account: &str,
        name: &str,
        display_name: &str,
        role: Option<FolderRole>,
    ) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO folders (account, name, role, display_name, present)
             VALUES (?1, ?2, ?3, ?4, 1)
             ON CONFLICT(account, name) DO UPDATE SET
               role = excluded.role,
               display_name = excluded.display_name,
               present = 1",
            params![account, name, FolderRole::to_column(role), display_name],
        )?;
        lookup_folder(&conn, account, name)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    /// 把该账户下**不在**本次 `LIST` 结果里的文件夹标记为缺席。
    ///
    /// 只标记，不删除：`LIST` 是文件夹清单的权威，不是删除用户邮件的授权
    /// （ADR-0026 决策 8）。返回被标记的行数。
    pub fn mark_absent_folders(
        &self,
        account: &str,
        present_names: &[String],
    ) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        if present_names.is_empty() {
            return conn.execute(
                "UPDATE folders SET present = 0 WHERE account = ?1 AND present = 1",
                params![account],
            );
        }
        let placeholders = (2..present_names.len() + 2)
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&account];
        for name in present_names {
            bound.push(name);
        }
        conn.execute(
            &format!(
                "UPDATE folders SET present = 0
                 WHERE account = ?1 AND present = 1 AND name NOT IN ({placeholders})"
            ),
            bound.as_slice(),
        )
    }

    /// 列出文件夹（账户内按 role 优先级、再按展示名）。`None` 列全部账户。
    pub fn list_folders(
        &self,
        account: Option<&str>,
    ) -> Result<Vec<FolderRecord>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        match account {
            Some(account) => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {FOLDER_COLUMNS} FROM folders WHERE account = ?1
                     ORDER BY account, display_name, id"
                ))?;
                let rows = stmt.query_map(params![account], decode_folder_row)?;
                rows.collect()
            }
            None => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {FOLDER_COLUMNS} FROM folders ORDER BY account, display_name, id"
                ))?;
                let rows = stmt.query_map([], decode_folder_row)?;
                rows.collect()
            }
        }
    }

    pub fn folder_by_name(
        &self,
        account: &str,
        name: &str,
    ) -> Result<Option<FolderRecord>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {FOLDER_COLUMNS} FROM folders WHERE account = ?1 AND name = ?2"),
            params![account, normalized_folder(name)],
            decode_folder_row,
        )
        .optional()
    }

    /// 文件夹内的消息总数与未读数。
    pub fn folder_counts(&self, folder_id: i64) -> Result<(i64, i64), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN seen = 0 THEN 1 ELSE 0 END), 0)
             FROM messages WHERE folder_id = ?1",
            params![folder_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    /// 推进增量游标。`uid_validity` 一并写入，使「已同步过」这件事有据可查。
    pub fn advance_folder_cursor(
        &self,
        folder_id: i64,
        uid_validity: u32,
        last_seen_uid: Option<u32>,
        synced_at: u64,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE folders SET uid_validity = ?2,
               last_seen_uid = MAX(COALESCE(last_seen_uid, 0), COALESCE(?3, 0)),
               synced_at = ?4
             WHERE id = ?1",
            params![
                folder_id,
                uid_validity as i64,
                last_seen_uid.map(|value| value as i64),
                synced_at as i64,
            ],
        )?;
        Ok(())
    }

    /// `UIDVALIDITY` 翻转：该文件夹的消息与附件删除、游标清零、写入新的 `uid_validity`。
    ///
    /// 单个 IMMEDIATE 事务内完成——中途崩溃整体回滚，下次连接重放（ADR-0026 决策 5）。
    /// 返回删除的消息行数。
    pub fn rebuild_folder(
        &self,
        folder_id: i64,
        uid_validity: u32,
    ) -> Result<usize, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM attachments WHERE (account, folder_id, uid) IN
               (SELECT account, folder_id, uid FROM messages WHERE folder_id = ?1)",
            params![folder_id],
        )?;
        let removed = tx.execute("DELETE FROM messages WHERE folder_id = ?1", params![folder_id])?;
        tx.execute(
            "UPDATE folders SET uid_validity = ?2, last_seen_uid = NULL WHERE id = ?1",
            params![folder_id, uid_validity as i64],
        )?;
        tx.commit()?;
        Ok(removed)
    }

    // —— 写路径与离线队列（ADR-0027） ——

    /// 乐观置位 + 入队，**同一个 IMMEDIATE 事务**。返回实际入队的条数
    /// （库里不存在的 uid 直接跳过）。
    pub fn queue_flag_ops(
        &self,
        account: &str,
        folder: &str,
        uids: &[String],
        flag: MessageFlag,
        value: bool,
        now: u64,
    ) -> Result<usize, rusqlite::Error> {
        Ok(self
            .queue_flag_receipt(account, folder, uids, flag, value, now)?
            .queued_uids
            .len())
    }

    pub fn queue_flag_receipt(
        &self,
        account: &str,
        folder: &str,
        uids: &[String],
        flag: MessageFlag,
        value: bool,
        now: u64,
    ) -> Result<WriteReceipt, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(folder_id) = lookup_folder(&tx, account, folder)? else {
            return Ok(WriteReceipt {
                missing_uids: uids.to_vec(),
                ..Default::default()
            });
        };
        let column = flag.column();
        let mut receipt = WriteReceipt::default();
        let mut visited = std::collections::HashSet::new();
        for uid in uids {
            if !visited.insert(uid) {
                continue;
            }
            let previous: Option<bool> = tx
                .query_row(
                    &format!(
                        "SELECT {column} FROM messages
                         WHERE account = ?1 AND folder_id = ?2 AND uid = ?3"
                    ),
                    params![account, folder_id, uid],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(previous) = previous else {
                receipt.missing_uids.push(uid.clone());
                continue;
            };
            if previous == value {
                receipt.unchanged_uids.push(uid.clone());
                continue;
            }
            tx.execute(
                &format!(
                    "UPDATE messages SET {column} = ?4, flags_local_at = ?5
                     WHERE account = ?1 AND folder_id = ?2 AND uid = ?3"
                ),
                params![account, folder_id, uid, value, now as i64],
            )?;
            let payload = serde_json::to_string(&FlagPayload {
                flag,
                value,
                previous,
            })
            .expect("载荷可序列化");
            insert_op(&tx, account, folder_id, uid, OpKind::Flag, &payload, now)?;
            receipt.queued_uids.push(uid.clone());
        }
        tx.commit()?;
        Ok(receipt)
    }

    /// 乐观移除 + 入队移动。源行立刻对列表不可见（ADR-0027 决策 5）。
    pub fn queue_move_ops(
        &self,
        account: &str,
        folder: &str,
        uids: &[String],
        target: &str,
        now: u64,
    ) -> Result<usize, rusqlite::Error> {
        Ok(self
            .queue_move_receipt(account, folder, uids, target, now)?
            .queued_uids
            .len())
    }

    pub fn queue_move_receipt(
        &self,
        account: &str,
        folder: &str,
        uids: &[String],
        target: &str,
        now: u64,
    ) -> Result<WriteReceipt, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(folder_id) = lookup_folder(&tx, account, folder)? else {
            return Ok(WriteReceipt {
                missing_uids: uids.to_vec(),
                ..Default::default()
            });
        };
        let payload = serde_json::to_string(&MovePayload {
            target: target.to_string(),
        })
        .expect("载荷可序列化");
        let mut receipt = WriteReceipt::default();
        let mut visited = std::collections::HashSet::new();
        for uid in uids {
            if !visited.insert(uid) {
                continue;
            }
            let affected = tx.execute(
                "UPDATE messages SET pending_removal = 1
                 WHERE account = ?1 AND folder_id = ?2 AND uid = ?3 AND pending_removal = 0",
                params![account, folder_id, uid],
            )?;
            if affected == 0 {
                receipt.missing_uids.push(uid.clone());
                continue;
            }
            insert_op(&tx, account, folder_id, uid, OpKind::Move, &payload, now)?;
            receipt.queued_uids.push(uid.clone());
        }
        tx.commit()?;
        Ok(receipt)
    }

    /// 入队一条「归档到已发送」（ADR-0028 决策 9）。
    ///
    /// `raw` 是**投递出去的那一份**报文——组装结果一定是纯 ASCII，因此可以原样进 TEXT 列。
    /// 账户没有已发送文件夹时返回 `Ok(false)`：没有归档目标，静默跳过而不是造一个。
    pub fn queue_append_op(
        &self,
        account: &str,
        sent_folder: &str,
        raw: &str,
        now: u64,
    ) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let Some(folder_id) = lookup_folder(&conn, account, sent_folder)? else {
            return Ok(false);
        };
        insert_op(&conn, account, folder_id, "", OpKind::Append, raw, now)?;
        Ok(true)
    }

    /// 崩溃或失联后停在 `in_flight` 的 op 与「命令发出未见响应」是同一种处境：
    /// 可重放的回到 `pending`，不可重放的落到 `unknown`（ADR-0027 决策 2）。
    pub fn recover_in_flight_ops(&self, account: &str, now: u64) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE pending_ops
             SET state = CASE WHEN kind = 'flag' THEN 'pending' ELSE 'unknown' END,
                 -- flag 幂等可重放；move / append 重放会产生重复副本。
                 detail = CASE WHEN kind = 'flag' THEN detail
                          ELSE '连接中断，命令是否送达未知' END,
                 updated_at = ?2
             WHERE account = ?1 AND state = 'in_flight'",
            params![account, now as i64],
        )
    }

    /// 取下一条待执行 op 并置为 `in_flight`。
    pub fn claim_next_op(
        &self,
        account: &str,
        now: u64,
    ) -> Result<Option<PendingOp>, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let claimed = tx
            .query_row(
                &format!(
                    "SELECT {PENDING_OP_COLUMNS} {PENDING_OP_FROM}
                     WHERE o.account = ?1 AND o.state = 'pending' ORDER BY o.id LIMIT 1"
                ),
                params![account],
                decode_pending_op_row,
            )
            .optional()?;
        let Some(mut op) = claimed else {
            tx.commit()?;
            return Ok(None);
        };
        tx.execute(
            "UPDATE pending_ops SET state = 'in_flight', attempts = attempts + 1, updated_at = ?2
             WHERE id = ?1",
            params![op.id, now as i64],
        )?;
        tx.commit()?;
        op.state = OpState::InFlight;
        op.attempts += 1;
        Ok(Some(op))
    }

    /// 落定一条 op，并施加它对本地状态的后果（ADR-0027 决策 2/5）。
    ///
    /// `Unknown` 对可重放的种类不是终态：回到 `pending` 自动重试，超过
    /// [`MAX_AUTO_ATTEMPTS`] 才转 `failed`。
    pub fn finish_op(
        &self,
        op: &PendingOp,
        outcome: OpState,
        detail: &str,
        now: u64,
    ) -> Result<OpState, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut settled = outcome;
        if outcome == OpState::Unknown && op.kind.replayable() {
            settled = if op.attempts >= MAX_AUTO_ATTEMPTS {
                OpState::Failed
            } else {
                OpState::Pending
            };
        }
        match (op.kind, settled) {
            (OpKind::Move, OpState::Done) => {
                // 目标文件夹会分配一个全新的 UID，源行到此为止。
                tx.execute(
                    "DELETE FROM attachments WHERE (account, folder_id, uid) IN
                       (SELECT account, folder_id, uid FROM messages
                        WHERE folder_id = ?1 AND uid = ?2)",
                    params![op.folder_id, op.uid],
                )?;
                tx.execute(
                    "DELETE FROM messages WHERE folder_id = ?1 AND uid = ?2",
                    params![op.folder_id, op.uid],
                )?;
            }
            (OpKind::Move, OpState::Failed) => {
                // 移动没成：消息回到源文件夹。
                tx.execute(
                    "UPDATE messages SET pending_removal = 0 WHERE folder_id = ?1 AND uid = ?2",
                    params![op.folder_id, op.uid],
                )?;
            }
            (OpKind::Flag, OpState::Failed) => {
                // 还原成用户点击之前的样子，并交出保护——让对账以服务器为准。
                if let Some(payload) = op.flag_payload() {
                    tx.execute(
                        &format!(
                            "UPDATE messages SET {} = ?3, flags_local_at = NULL
                             WHERE folder_id = ?1 AND uid = ?2",
                            payload.flag.column()
                        ),
                        params![op.folder_id, op.uid, payload.previous],
                    )?;
                }
            }
            _ => {}
        }
        tx.execute(
            "UPDATE pending_ops SET state = ?2, detail = ?3, updated_at = ?4 WHERE id = ?1",
            params![op.id, settled.as_str(), detail, now as i64],
        )?;
        tx.commit()?;
        Ok(settled)
    }

    /// 列出 op。`only_open` 为真时只返回未落定的（`pending`/`in_flight`/`unknown`）。
    pub fn list_pending_ops(
        &self,
        account: Option<&str>,
        only_open: bool,
        limit: usize,
    ) -> Result<Vec<PendingOp>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut clauses: Vec<String> = Vec::new();
        let mut bound: Vec<String> = Vec::new();
        if let Some(account) = account {
            bound.push(account.to_string());
            clauses.push(format!("o.account = ?{}", bound.len()));
        }
        if only_open {
            clauses.push("o.state IN ('pending', 'in_flight', 'unknown')".into());
        }
        let filter = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let limit = limit as i64;
        let mut stmt = conn.prepare(&format!(
            "SELECT {PENDING_OP_COLUMNS} {PENDING_OP_FROM} {filter}
             ORDER BY o.id DESC LIMIT ?{}",
            bound.len() + 1
        ))?;
        let mut params: Vec<&dyn rusqlite::ToSql> =
            bound.iter().map(|value| value as &dyn rusqlite::ToSql).collect();
        params.push(&limit);
        let rows = stmt.query_map(params.as_slice(), decode_pending_op_row)?;
        rows.collect()
    }

    /// 用户裁决一条已落定但未成功的 op。
    ///
    /// `retry` = 重新入队；否则放弃——清掉乐观移除标记，让存在性对账收敛真相
    /// （ADR-0027 决策 5/6）。
    pub fn resolve_op(
        &self,
        id: i64,
        retry: bool,
        now: u64,
    ) -> Result<Option<PendingOp>, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                &format!("SELECT {PENDING_OP_COLUMNS} {PENDING_OP_FROM} WHERE o.id = ?1"),
                params![id],
                decode_pending_op_row,
            )
            .optional()?;
        let Some(op) = existing else {
            tx.commit()?;
            return Ok(None);
        };
        if matches!(op.state, OpState::Pending | OpState::InFlight | OpState::Done) {
            // 还没落定、或者已经成功的 op 不接受裁决。
            tx.commit()?;
            return Ok(Some(op));
        }
        if retry {
            tx.execute(
                "UPDATE pending_ops SET state = 'pending', attempts = 0, detail = '',
                   updated_at = ?2 WHERE id = ?1",
                params![id, now as i64],
            )?;
        } else {
            tx.execute(
                "UPDATE pending_ops SET state = 'failed', detail = '用户放弃', updated_at = ?2
                 WHERE id = ?1",
                params![id, now as i64],
            )?;
            if op.kind == OpKind::Move {
                tx.execute(
                    "UPDATE messages SET pending_removal = 0 WHERE folder_id = ?1 AND uid = ?2",
                    params![op.folder_id, op.uid],
                )?;
            }
        }
        let updated = tx.query_row(
            &format!("SELECT {PENDING_OP_COLUMNS} {PENDING_OP_FROM} WHERE o.id = ?1"),
            params![id],
            decode_pending_op_row,
        )?;
        tx.commit()?;
        Ok(Some(updated))
    }

    /// 与服务器对账标记位。
    ///
    /// 两条护栏（ADR-0027 决策 6）：最近 `protect_after` 之后有本地乐观改写的行跳过；
    /// 任何还有未落定 op 的 `(folder, uid)` 跳过。`full` 为真（本轮拿到了完整 UID 集合）
    /// 时顺带做存在性对账——本地有、服务器没有的行删除。
    pub fn reconcile_flags(
        &self,
        folder_id: i64,
        observed: &[(u32, ObservedFlags)],
        full: bool,
        protect_after: u64,
    ) -> Result<ReconcileOutcome, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut protected_uids: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            let mut stmt = tx.prepare(
                "SELECT uid FROM pending_ops
                 WHERE folder_id = ?1 AND state IN ('pending', 'in_flight', 'unknown')",
            )?;
            let rows = stmt.query_map(params![folder_id], |row| row.get::<_, String>(0))?;
            for uid in rows {
                protected_uids.insert(uid?);
            }
        }

        let mut outcome = ReconcileOutcome::default();
        for (uid, flags) in observed {
            let uid = uid.to_string();
            if protected_uids.contains(&uid) {
                outcome.protected += 1;
                continue;
            }
            let updated = tx.execute(
                "UPDATE messages
                 SET seen = ?3, flagged = ?4, answered = ?5, deleted = ?6
                 WHERE folder_id = ?1 AND uid = ?2
                   AND (flags_local_at IS NULL OR flags_local_at <= ?7)
                   AND (seen != ?3 OR flagged != ?4 OR answered != ?5 OR deleted != ?6)",
                params![
                    folder_id,
                    uid,
                    flags.seen,
                    flags.flagged,
                    flags.answered,
                    flags.deleted,
                    protect_after as i64,
                ],
            )?;
            outcome.updated += updated;
        }

        if full {
            let mut existing: Vec<String> = Vec::new();
            {
                let mut stmt = tx.prepare(
                    "SELECT uid FROM messages
                     WHERE folder_id = ?1
                       AND (flags_local_at IS NULL OR flags_local_at <= ?2)",
                )?;
                let rows = stmt.query_map(params![folder_id, protect_after as i64], |row| {
                    row.get::<_, String>(0)
                })?;
                for uid in rows {
                    existing.push(uid?);
                }
            }
            let seen_uids: std::collections::HashSet<String> =
                observed.iter().map(|(uid, _)| uid.to_string()).collect();
            for uid in existing {
                if seen_uids.contains(&uid) {
                    continue;
                }
                if protected_uids.contains(&uid) {
                    outcome.protected += 1;
                    continue;
                }
                // 服务器上已经没有它了：其他客户端 EXPUNGE 过。
                tx.execute(
                    "DELETE FROM attachments WHERE (account, folder_id, uid) IN
                       (SELECT account, folder_id, uid FROM messages
                        WHERE folder_id = ?1 AND uid = ?2)",
                    params![folder_id, uid],
                )?;
                outcome.removed += tx.execute(
                    "DELETE FROM messages WHERE folder_id = ?1 AND uid = ?2",
                    params![folder_id, uid],
                )?;
            }
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// 记录 CONDSTORE 的 `HIGHESTMODSEQ`（ADR-0027 决策 7）。
    pub fn set_folder_modseq(
        &self,
        folder_id: i64,
        modseq: Option<u64>,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE folders SET highest_modseq = ?2 WHERE id = ?1",
            params![folder_id, modseq.map(|value| value as i64)],
        )?;
        Ok(())
    }

    /// 该文件夹上次记录的 `HIGHESTMODSEQ`。
    pub fn folder_modseq(&self, folder_id: i64) -> Result<Option<u64>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT highest_modseq FROM folders WHERE id = ?1",
            params![folder_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map(|value| value.map(|value| value as u64))
    }

    // —— 地址层（ADR-0022） ——

    /// 在同一 IMMEDIATE 事务中执行单账户上限检查与插入，避免并发创建
    /// 越过上限。唯一地址冲突与上限命中分别返回稳定结果。
    pub(crate) fn insert_address_bounded(
        &self,
        record: &AddressRecord,
        max_per_account: usize,
    ) -> Result<InsertAddressOutcome, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count = tx.query_row(
            "SELECT COUNT(*) FROM addresses WHERE account = ?1",
            params![record.account],
            |row| row.get::<_, i64>(0),
        )?;
        if count >= max_per_account as i64 {
            return Ok(InsertAddressOutcome::LimitReached);
        }
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO addresses (id, account, address, kind, label, state, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.id,
                record.account,
                record.address,
                record.kind.as_str(),
                record.label,
                record.state.as_str(),
                record.created_at as i64,
                record.expires_at.map(|value| value as i64),
            ],
        )?;
        tx.commit()?;
        Ok(if inserted > 0 {
            InsertAddressOutcome::Inserted
        } else {
            InsertAddressOutcome::Duplicate
        })
    }

    /// 按父账户列出地址（id 创建序）；`None` 列全部。
    pub(crate) fn list_addresses(
        &self,
        account: Option<&str>,
    ) -> Result<Vec<AddressRecord>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        match account {
            Some(account) => {
                let mut stmt = conn.prepare(
                    "SELECT id, account, address, kind, label, state, created_at, expires_at
                     FROM addresses WHERE account = ?1 ORDER BY created_at, id",
                )?;
                let rows = stmt.query_map(params![account], decode_address_row)?;
                rows.collect()
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, account, address, kind, label, state, created_at, expires_at
                     FROM addresses ORDER BY created_at, id",
                )?;
                let rows = stmt.query_map([], decode_address_row)?;
                rows.collect()
            }
        }
    }

    #[cfg(test)]
    fn get_address(&self, id: &str) -> Result<Option<AddressRecord>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, account, address, kind, label, state, created_at, expires_at
             FROM addresses WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(decode_address_row(row)?))
    }

    /// 在同一 IMMEDIATE 事务中完成惰性过期、状态门禁与字段 patch。
    /// 省略字段绝不用先前读取的快照回写，避免并发 label/state patch
    /// 互相覆盖；过期行的 label 仍可改，但状态不可复活。
    pub(crate) fn patch_address(
        &self,
        id: &str,
        state: Option<AddressState>,
        label: Option<&str>,
        now_unix_secs: u64,
    ) -> Result<PatchAddressOutcome, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE addresses SET state = 'expired'
             WHERE id = ?1 AND state != 'expired'
               AND expires_at IS NOT NULL AND expires_at <= ?2",
            params![id, now_unix_secs as i64],
        )?;
        let existing = tx
            .query_row(
                "SELECT id, account, address, kind, label, state, created_at, expires_at
                 FROM addresses WHERE id = ?1",
                params![id],
                decode_address_row,
            )
            .optional()?;
        let Some(existing) = existing else {
            tx.commit()?;
            return Ok(PatchAddressOutcome::NotFound);
        };
        if state.is_some() && existing.state == AddressState::Expired {
            tx.commit()?;
            return Ok(PatchAddressOutcome::Expired);
        }

        match (state, label) {
            (Some(state), Some(label)) => tx.execute(
                "UPDATE addresses SET state = ?2, label = ?3 WHERE id = ?1",
                params![id, state.as_str(), label],
            )?,
            (Some(state), None) => tx.execute(
                "UPDATE addresses SET state = ?2 WHERE id = ?1",
                params![id, state.as_str()],
            )?,
            (None, Some(label)) => tx.execute(
                "UPDATE addresses SET label = ?2 WHERE id = ?1",
                params![id, label],
            )?,
            (None, None) => 0,
        };
        let updated = tx.query_row(
            "SELECT id, account, address, kind, label, state, created_at, expires_at
             FROM addresses WHERE id = ?1",
            params![id],
            decode_address_row,
        )?;
        tx.commit()?;
        Ok(PatchAddressOutcome::Updated(updated))
    }

    /// 删除地址行；消息里的 recipient 成为悬垂文本（ADR-0022 §7）。
    pub(crate) fn delete_address(&self, id: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM addresses WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// 删除某账户的全部地址（账户移除级联）。
    pub(crate) fn delete_addresses_for_account(
        &self,
        account: &str,
    ) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM addresses WHERE account = ?1", params![account])
    }

    /// 单账户地址计数（上限护栏用）。
    pub fn count_addresses(&self, account: &str) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM addresses WHERE account = ?1",
            params![account],
            |row| row.get(0),
        )
    }

    /// 惰性过期回写：active 且已到期的行置为 expired，返回受影响行数。
    /// 无定时器——由读取/使用路径顺带调用（ADR-0022 §4）。
    pub(crate) fn expire_due_addresses(
        &self,
        now_unix_secs: u64,
    ) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE addresses SET state = 'expired'
             WHERE state != 'expired' AND expires_at IS NOT NULL AND expires_at <= ?1",
            params![now_unix_secs as i64],
        )
    }

    // —— 草稿（ADR-0034）——

    /// 新建一份草稿。`id` 由调用方生成（`drafts::generate_draft_id`）。
    ///
    /// 整件事在一个 immediate 事务里：附件引用的校验、行插入与计数上限都必须与并发的
    /// 另一次创建互斥，否则两个前端能同时把第 200 份草稿写进去。
    pub fn create_draft(
        &self,
        id: &str,
        account: &str,
        patch: &DraftPatch,
        now: u64,
    ) -> Result<DraftWrite, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM drafts WHERE account = ?1",
            params![account],
            |row| row.get(0),
        )?;
        if existing >= MAX_DRAFTS_PER_ACCOUNT {
            return Ok(DraftWrite::LimitReached {
                limit: MAX_DRAFTS_PER_ACCOUNT,
            });
        }
        let mut draft = Draft {
            id: id.to_owned(),
            account: account.to_owned(),
            send_state: "editing".into(),
            version: 1,
            created_at: now,
            updated_at: now,
            ..Draft::default()
        };
        patch.apply_fields(&mut draft);
        tx.execute(
            "INSERT INTO drafts (id, account, from_address, to_addrs, cc_addrs, bcc_addrs,
                                 subject, body, body_html, in_reply_to, reference_ids,
                                 version, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, ?12, ?12)",
            params![
                draft.id,
                draft.account,
                draft.from,
                encode_addresses(&draft.to),
                encode_addresses(&draft.cc),
                encode_addresses(&draft.bcc),
                draft.subject,
                draft.body,
                draft.body_html,
                draft.in_reply_to,
                draft.references,
                now as i64,
            ],
        )?;
        match write_draft_attachments(&tx, &draft.id, account, patch.attachments.as_deref())? {
            Ok(attachments) => draft.attachments = attachments,
            Err(rejection) => return Ok(rejection),
        }
        tx.commit()?;
        Ok(DraftWrite::Saved(Box::new(draft)))
    }

    /// 按 `base_version` 更新一份草稿（ADR-0034 决策 3）。版本不符时**一个字节都不写**。
    pub fn update_draft(
        &self,
        id: &str,
        base_version: u64,
        patch: &DraftPatch,
        now: u64,
    ) -> Result<DraftWrite, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut draft) = load_draft(&tx, id)? else {
            return Ok(DraftWrite::NotFound);
        };
        if draft.version != base_version {
            return Ok(DraftWrite::Conflict {
                version: draft.version,
            });
        }
        if !matches!(draft.send_state.as_str(), "editing" | "failed") {
            return Ok(DraftWrite::SendLocked);
        }
        if let Some(account) = &patch.account {
            if account != &draft.account
                && tx.query_row(
                    "SELECT count(*) FROM drafts WHERE account=?1",
                    [account],
                    |r| r.get::<_, i64>(0),
                )? >= MAX_DRAFTS_PER_ACCOUNT
            {
                return Ok(DraftWrite::LimitReached {
                    limit: MAX_DRAFTS_PER_ACCOUNT,
                });
            }
        }
        patch.apply_fields(&mut draft);
        draft.version += 1;
        draft.updated_at = now;
        tx.execute(
            "UPDATE drafts SET from_address = ?2, to_addrs = ?3, cc_addrs = ?4, bcc_addrs = ?5,
                               subject = ?6, body = ?7, body_html = ?8, in_reply_to = ?9,
                               reference_ids = ?10, version = ?11, updated_at = ?12, account = ?14
             WHERE id = ?1 AND version = ?13",
            params![
                draft.id,
                draft.from,
                encode_addresses(&draft.to),
                encode_addresses(&draft.cc),
                encode_addresses(&draft.bcc),
                draft.subject,
                draft.body,
                draft.body_html,
                draft.in_reply_to,
                draft.references,
                draft.version as i64,
                now as i64,
                base_version as i64,
                draft.account,
            ],
        )?;
        if patch.attachments.is_some() {
            match write_draft_attachments(
                &tx,
                &draft.id,
                &draft.account,
                patch.attachments.as_deref(),
            )? {
                Ok(attachments) => draft.attachments = attachments,
                Err(rejection) => return Ok(rejection),
            }
        } else {
            draft.attachments = read_draft_attachments(&tx, &draft.id)?;
        }
        tx.commit()?;
        Ok(DraftWrite::Saved(Box::new(draft)))
    }

    /// 读一份完整草稿。附件引用的 `stored` 是**现查的**当前状态：源邮件可能已经被删掉。
    pub fn get_draft(&self, id: &str) -> Result<Option<Draft>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let Some(mut draft) = load_draft(&conn, id)? else {
            return Ok(None);
        };
        draft.attachments = read_draft_attachments(&conn, &draft.id)?;
        Ok(Some(draft))
    }

    /// 列出草稿摘要，最近更新在前。正文不进列表，只给单行摘要。
    pub fn list_drafts(
        &self,
        account: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<DraftSummary>, i64), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let clause = if account.is_some() { "WHERE d.account = ?1" } else { "" };
        let binding: Vec<&dyn rusqlite::ToSql> = match &account {
            Some(account) => vec![account],
            None => Vec::new(),
        };
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM drafts d {clause}"),
            binding.as_slice(),
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {DRAFT_COLUMNS},
                    (SELECT COUNT(*) FROM draft_attachments a WHERE a.draft_id = d.id)
             FROM drafts d {clause}
             ORDER BY d.updated_at DESC, d.id
             LIMIT {limit} OFFSET {offset}"
        ))?;
        let rows = stmt.query_map(binding.as_slice(), |row| {
            let draft = decode_draft_row(row)?;
            Ok(DraftSummary {
                snippet: crate::drafts::snippet_of(&draft),
                attachment_count: row.get::<_, i64>(16)? as u32,
                id: draft.id,
                account: draft.account,
                from: draft.from,
                to: draft.to,
                subject: draft.subject,
                version: draft.version,
                created_at: draft.created_at,
                updated_at: draft.updated_at,
            })
        })?;
        let mut summaries = Vec::new();
        for row in rows {
            summaries.push(row?);
        }
        Ok((summaries, total))
    }

    /// 丢弃一份草稿。`base_version` 为 `None` 时无条件删除——「丢弃」是用户看着这份草稿
    /// 做的决定；带上版本号则是「除非它在别处改过」。
    pub fn delete_draft(
        &self,
        id: &str,
        base_version: Option<u64>,
    ) -> Result<DraftWrite, rusqlite::Error> {
        let mut uploads = self.uploads.lock().unwrap();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut draft) = load_draft(&tx, id)? else {
            return Ok(DraftWrite::NotFound);
        };
        if base_version.is_some_and(|expected| expected != draft.version) {
            return Ok(DraftWrite::Conflict {
                version: draft.version,
            });
        }
        if draft.send_state == "sending" {
            return Ok(DraftWrite::SendLocked);
        }
        draft.attachments = read_draft_attachments(&tx, id)?;
        tx.execute("DELETE FROM draft_attachments WHERE draft_id = ?1", params![id])?;
        tx.execute("DELETE FROM drafts WHERE id = ?1", params![id])?;
        tx.commit()?;
        uploads.discard_draft(id);
        Ok(DraftWrite::Saved(Box::new(draft)))
    }

    /// 某账户当前的草稿份数（上限检查与 CLI 用）。
    pub fn count_drafts(&self, account: &str) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM drafts WHERE account = ?1",
            params![account],
            |row| row.get(0),
        )
    }
}

/// store 插件：inject 配置 → 打开库 → provide Store → 订阅 MailFiltered 落库。
pub struct StorePlugin;

impl Plugin for StorePlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<StoreConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let cfg = ctx.get::<StoreConfig>().expect("inject 就绪");
        let store = Arc::new(Store::open(&cfg.db_path)?);
        ctx.provide::<Store>(Arc::clone(&store));
        spawn_search_backfill(ctx, &store);
        uploads::spawn_cleanup(ctx, &store);
        let s = store;
        ctx.on_emit_scoped::<MailFiltered>(move |msg| {
            if let Err(e) = s.insert_message(&msg) {
                eprintln!("[maild/store] 落库失败: {e}");
            }
        });
        Ok(())
    }
}

/// 一批回填的条数与批间停顿。约 1000 封/秒——足够快到升级当天就搜得到，
/// 又慢到不会和收信争数据库。
const SEARCH_BACKFILL_BATCH: usize = 200;
const SEARCH_BACKFILL_PAUSE: std::time::Duration = std::time::Duration::from_millis(200);
/// 卸载时等待在途批次收尾的上限。一批就是一个小事务，2 秒是宽裕的上界。
const SEARCH_BACKFILL_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// 启动索引回填的后台任务（ADR-0029 决策 9）。
///
/// 任务只持 `Weak<Store>`，每批临时 upgrade——这是 ADR-0026 决策 11 的直接应用：
/// 游离任务把 `Arc<Store>` 攥过 await 点，就等于把数据库锁一起攥住，daemon 停不下来。
///
/// 光有 `Weak` 还不够：一批正在 `spawn_blocking` 里跑时，那个闭包持有的是真 `Arc`。
/// 所以卸载逆是**异步**的——先发停止信号，再等在途批次收尾（有界），确认没有游离
/// 引用之后才让 dispose 继续。只 abort 不等待会把数据库锁扣在一个已经没人引用的
/// 阻塞任务里，下一次 `Store::open` 就打不开。
///
/// 没有 Tokio runtime 时（直接 `Store::open` 的单元测试）不启动，回填只在显式调用时推进。
fn spawn_search_backfill(ctx: &Context, store: &Arc<Store>) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let weak = Arc::downgrade(store);
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let task = handle.spawn(async move {
        loop {
            if *stop_rx.borrow() {
                return;
            }
            let Some(store) = weak.upgrade() else {
                return;
            };
            let outcome =
                tokio::task::spawn_blocking(move || store.backfill_search_index(SEARCH_BACKFILL_BATCH))
                    .await;
            match outcome {
                Ok(Ok(progress)) if progress.done => {
                    // 回填是启动后最大的一段写入：结束时收紧页缓存 / WAL，并把空闲堆页还给内核。
                    if let Some(store) = weak.upgrade() {
                        let _ = tokio::task::spawn_blocking(move || store.shrink_memory()).await;
                    }
                    crate::daemon::release_unused_memory();
                    return;
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    // 持续性的库错误重试也是同一个结果，停下并留下痕迹。
                    eprintln!("[maild/store] 检索索引回填失败: {error}");
                    return;
                }
                Err(_) => return,
            }
            tokio::select! {
                _ = tokio::time::sleep(SEARCH_BACKFILL_PAUSE) => {}
                _ = stop_rx.changed() => return,
            }
        }
    });
    ctx.effect_async(async move {
        let _ = stop_tx.send_replace(true);
        let mut task = task;
        if tokio::time::timeout(SEARCH_BACKFILL_STOP_TIMEOUT, &mut task)
            .await
            .is_err()
        {
            task.abort();
            eprintln!("[maild/store] 检索索引回填未在期限内停止，已放弃等待");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(id: &str, value: &str) -> AddressRecord {
        AddressRecord {
            id: id.into(),
            account: "acct_test".into(),
            address: value.into(),
            kind: AddressKind::Alias,
            label: String::new(),
            state: AddressState::Active,
            created_at: 1,
            expires_at: None,
        }
    }

    #[test]
    fn address_insert_limit_is_atomic_and_unknown_state_fails_closed() {
        let store = Store::open("").unwrap();
        let first = address("addr_first", "first@example.test");
        let second = address("addr_second", "second@example.test");
        assert_eq!(
            store.insert_address_bounded(&first, 1).unwrap(),
            InsertAddressOutcome::Inserted
        );
        assert_eq!(
            store.insert_address_bounded(&second, 1).unwrap(),
            InsertAddressOutcome::LimitReached
        );

        // 模拟来自历史/外部 SQLite 的语义坏值。即使绕过 DB CHECK，读取也
        // 必须报错，绝不能把未来状态默认成 Active 后允许发信。
        {
            let conn = store.conn.lock().unwrap();
            conn.execute_batch("PRAGMA ignore_check_constraints=ON;")
                .unwrap();
            conn.execute(
                "UPDATE addresses SET state = 'future-state' WHERE id = ?1",
                params![first.id],
            )
            .unwrap();
        }
        assert!(store.list_addresses(None).is_err());
        assert!(store.get_address(&first.id).is_err());
    }

    #[test]
    fn recent_contact_query_uses_account_sort_index_without_temp_btree() {
        let store = Store::open("").unwrap();
        let conn = store.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT m.id, m.sender, m.recipient, m.cc, m.sort_at, f.role
                 FROM messages m
                 JOIN folders f ON f.id = m.folder_id AND f.account = m.account
                 WHERE m.account = ?1
                   AND m.pending_removal = 0
                   AND m.deleted = 0
                   AND f.role NOT IN ('spam', 'trash', 'drafts')
                 ORDER BY m.sort_at DESC, m.id DESC
                 LIMIT ?2",
            )
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map(params!["acc", 800_i64], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let joined = plan.join("\n");
        assert!(
            joined.contains("idx_messages_account_sort"),
            "近期样本查询必须走 idx_messages_account_sort: {joined}"
        );
        assert!(
            !joined.contains("USE TEMP B-TREE FOR ORDER BY"),
            "近期样本查询不得引入临时 B-Tree 排序: {joined}"
        );
    }
}
