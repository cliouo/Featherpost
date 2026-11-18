use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension as _, TransactionBehavior};

use crate::error::{AppError, AppResult, ErrorCode};
use crate::PLUGIN_ID;

const STATE_FILE: &str = "nodeseek-email.sqlite3";
const SCHEMA_VERSION: i64 = 1;
const MAX_REMOTE_IDS_PER_ASSIGNMENT: usize = 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailboxBinding {
    pub account: String,
    pub address: String,
    pub mailbox_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingMail {
    pub remote_mail_id: i64,
    pub synthetic_uid: u64,
}

#[derive(Clone, Debug)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn discover() -> AppResult<Self> {
        let root = match std::env::var_os(maild_paths::PLUGIN_DATA_ENV) {
            Some(value) if !value.is_empty() => {
                let value = PathBuf::from(value);
                if !value.is_absolute() {
                    return Err(AppError::state());
                }
                value
            }
            Some(_) => return Err(AppError::state()),
            None => maild_paths::AppPaths::discover()
                .map_err(|_| AppError::state())?
                .plugin_data_dir
                .join(PLUGIN_ID),
        };
        Self::at_root(root)
    }

    pub fn at_root(root: PathBuf) -> AppResult<Self> {
        ensure_private_directory(&root)?;
        let store = Self {
            path: root.join(STATE_FILE),
        };
        let _ = store.open_checked()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn import_mailbox(
        &self,
        account: &str,
        address: &str,
        mailbox_id: i64,
    ) -> AppResult<MailboxBinding> {
        validate_account(account)?;
        let address = canonicalize_address(address)?;
        if mailbox_id <= 0 {
            return Err(AppError::invalid_input());
        }
        let mut connection = self.open_checked()?;
        let transaction = connection.transaction().map_err(|_| AppError::state())?;
        if let Some(existing) = binding_in(&transaction, account)? {
            if existing.address == address && existing.mailbox_id == mailbox_id {
                transaction.commit().map_err(|_| AppError::state())?;
                return Ok(existing);
            }
            return Err(AppError::new(
                ErrorCode::Conflict,
                "账户已经绑定到另一个远端邮箱",
            ));
        }
        let address_owner: Option<String> = transaction
            .query_row(
                "SELECT account_id FROM account_mailboxes WHERE address = ?1",
                params![address],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::state())?;
        if address_owner.is_some() {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "远端邮箱已经绑定到另一个账户",
            ));
        }
        transaction
            .execute(
                "INSERT INTO account_mailboxes(account_id, address, mailbox_id) VALUES (?1, ?2, ?3)",
                params![account, address, mailbox_id],
            )
            .map_err(|_| AppError::state())?;
        transaction.commit().map_err(|_| AppError::state())?;
        Ok(MailboxBinding {
            account: account.to_owned(),
            address,
            mailbox_id,
        })
    }

    pub fn binding(&self, account: &str) -> AppResult<MailboxBinding> {
        validate_account(account)?;
        let connection = self.open_checked()?;
        binding_in(&connection, account)?
            .ok_or_else(|| AppError::new(ErrorCode::NotFound, "账户尚未导入 NodeSeek Email 邮箱"))
    }

    pub fn binding_for_address(&self, address: &str) -> AppResult<Option<MailboxBinding>> {
        let address = canonicalize_address(address)?;
        let connection = self.open_checked()?;
        binding_for_address_in(&connection, &address)
    }

    /// Remove one local mailbox binding and its synthetic-UID state.
    ///
    /// This method never contacts NodeSeek or deletes the remote mailbox. It is intended only for
    /// an account that has already been archived in the host; reusing it for an active account would
    /// reset that account's provider cursor namespace.
    pub fn unimport_mailbox(&self, account: &str, address: &str) -> AppResult<MailboxBinding> {
        validate_account(account)?;
        let address = canonicalize_address(address)?;
        let mut connection = self.open_checked()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AppError::state())?;
        let binding = binding_in(&transaction, account)?.ok_or_else(|| {
            AppError::new(ErrorCode::NotFound, "账户尚未导入 NodeSeek Email 邮箱")
        })?;
        if binding.address != address {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "账户绑定地址与待解除地址不一致",
            ));
        }
        let removed = transaction
            .execute(
                "DELETE FROM account_mailboxes WHERE account_id = ?1 AND address = ?2",
                params![account, address],
            )
            .map_err(|_| AppError::state())?;
        if removed != 1 {
            return Err(AppError::state());
        }
        transaction.commit().map_err(|_| AppError::state())?;
        Ok(binding)
    }

    pub fn assign_remote_ids(&self, account: &str, remote_ids: &[i64]) -> AppResult<()> {
        validate_account(account)?;
        if remote_ids.len() > MAX_REMOTE_IDS_PER_ASSIGNMENT || remote_ids.iter().any(|id| *id <= 0)
        {
            return Err(AppError::invalid_response());
        }
        let mut ids = remote_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();

        let mut connection = self.open_checked()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AppError::state())?;
        if binding_in(&transaction, account)?.is_none() {
            return Err(AppError::new(
                ErrorCode::NotFound,
                "账户尚未导入 NodeSeek Email 邮箱",
            ));
        }
        if ids.is_empty() {
            transaction.commit().map_err(|_| AppError::state())?;
            return Ok(());
        }

        // The API page is deliberately small. Bind it as one bounded VALUES relation so both
        // existence checks and insertion stay set-based and use the account/remote-id index.
        let values = (0..ids.len())
            .map(|index| format!("(?{})", index + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let cte = format!(
            "WITH incoming(remote_mail_id) AS (VALUES {values}), \
             new_ids(remote_mail_id) AS (\
                SELECT DISTINCT incoming.remote_mail_id FROM incoming \
                LEFT JOIN mail_uid_map AS existing \
                  ON existing.account_id = ?1 \
                 AND existing.remote_mail_id = incoming.remote_mail_id \
                WHERE existing.remote_mail_id IS NULL\
             )"
        );
        let mut bindings = Vec::with_capacity(ids.len() + 1);
        bindings.push(SqlValue::Text(account.to_owned()));
        bindings.extend(ids.iter().copied().map(SqlValue::Integer));

        let bounds_sql = format!(
            "{cte} \
             SELECT (\
                       SELECT COALESCE(MAX(synthetic_uid), 0) \
                       FROM mail_uid_map WHERE account_id = ?1\
                    ), COUNT(*) \
             FROM new_ids"
        );
        let (highest_uid, missing_count): (i64, i64) = transaction
            .query_row(&bounds_sql, params_from_iter(bindings.iter()), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(|_| AppError::state())?;
        highest_uid
            .checked_add(missing_count)
            .ok_or_else(AppError::state)?;

        let insert_sql = format!(
            "{cte} \
             INSERT INTO mail_uid_map(account_id, remote_mail_id, synthetic_uid) \
             SELECT ?1, new_ids.remote_mail_id, \
                    ?{} + ROW_NUMBER() OVER (ORDER BY new_ids.remote_mail_id) \
             FROM new_ids",
            ids.len() + 2
        );
        let mut insert_bindings = bindings;
        insert_bindings.push(SqlValue::Integer(highest_uid));
        let inserted = transaction
            .execute(&insert_sql, params_from_iter(insert_bindings.iter()))
            .map_err(|_| AppError::state())?;
        if inserted != usize::try_from(missing_count).map_err(|_| AppError::state())? {
            return Err(AppError::state());
        }
        transaction.commit().map_err(|_| AppError::state())?;
        Ok(())
    }

    pub fn pending(&self, account: &str, since: u64, max: usize) -> AppResult<Vec<PendingMail>> {
        validate_account(account)?;
        if max == 0 || max > 20 || since > i64::MAX as u64 {
            return Err(AppError::invalid_input());
        }
        let connection = self.open_checked()?;
        if binding_in(&connection, account)?.is_none() {
            return Err(AppError::new(
                ErrorCode::NotFound,
                "账户尚未导入 NodeSeek Email 邮箱",
            ));
        }
        let highest: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(synthetic_uid), 0) FROM mail_uid_map WHERE account_id = ?1",
                params![account],
                |row| row.get(0),
            )
            .map_err(|_| AppError::state())?;
        if since > highest as u64 {
            return Err(AppError::new(ErrorCode::State, "接收游标超出本地持久映射"));
        }
        let mut statement = connection
            .prepare(
                "SELECT remote_mail_id, synthetic_uid FROM mail_uid_map \
                 WHERE account_id = ?1 AND synthetic_uid > ?2 AND tombstoned = 0 \
                 ORDER BY synthetic_uid ASC LIMIT ?3",
            )
            .map_err(|_| AppError::state())?;
        let rows = statement
            .query_map(params![account, since as i64, max as i64], |row| {
                let uid: i64 = row.get(1)?;
                Ok((row.get::<_, i64>(0)?, uid))
            })
            .map_err(|_| AppError::state())?;
        let mut out = Vec::new();
        for row in rows {
            let (remote_mail_id, uid) = row.map_err(|_| AppError::state())?;
            let synthetic_uid = u64::try_from(uid).map_err(|_| AppError::state())?;
            out.push(PendingMail {
                remote_mail_id,
                synthetic_uid,
            });
        }
        Ok(out)
    }

    pub fn scan_offset(&self, account: &str) -> AppResult<usize> {
        validate_account(account)?;
        let connection = self.open_checked()?;
        let value: i64 = connection
            .query_row(
                "SELECT scan_offset FROM account_mailboxes WHERE account_id = ?1",
                params![account],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::state())?
            .ok_or_else(|| {
                AppError::new(ErrorCode::NotFound, "账户尚未导入 NodeSeek Email 邮箱")
            })?;
        usize::try_from(value).map_err(|_| AppError::state())
    }

    pub fn set_scan_offset(&self, account: &str, offset: usize) -> AppResult<()> {
        validate_account(account)?;
        let offset = i64::try_from(offset).map_err(|_| AppError::invalid_input())?;
        let connection = self.open_checked()?;
        let changed = connection
            .execute(
                "UPDATE account_mailboxes SET scan_offset = ?2 WHERE account_id = ?1",
                params![account, offset],
            )
            .map_err(|_| AppError::state())?;
        if changed != 1 {
            return Err(AppError::new(
                ErrorCode::NotFound,
                "账户尚未导入 NodeSeek Email 邮箱",
            ));
        }
        Ok(())
    }

    pub fn mark_remote_missing(&self, account: &str, remote_mail_id: i64) -> AppResult<()> {
        validate_account(account)?;
        if remote_mail_id <= 0 {
            return Err(AppError::invalid_input());
        }
        let connection = self.open_checked()?;
        let changed = connection
            .execute(
                "UPDATE mail_uid_map SET tombstoned = 1 \
                 WHERE account_id = ?1 AND remote_mail_id = ?2",
                params![account, remote_mail_id],
            )
            .map_err(|_| AppError::state())?;
        if changed != 1 {
            return Err(AppError::state());
        }
        Ok(())
    }

    fn open_checked(&self) -> AppResult<Connection> {
        ensure_state_file(&self.path)?;
        let connection = Connection::open(&self.path).map_err(|_| AppError::state())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| AppError::state())?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys=ON; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA temp_store=MEMORY;",
            )
            .map_err(|_| AppError::state())?;
        initialize_or_validate_schema(&connection)?;
        Ok(connection)
    }
}

fn binding_in(connection: &Connection, account: &str) -> AppResult<Option<MailboxBinding>> {
    connection
        .query_row(
            "SELECT account_id, address, mailbox_id FROM account_mailboxes WHERE account_id = ?1",
            params![account],
            |row| {
                Ok(MailboxBinding {
                    account: row.get(0)?,
                    address: row.get(1)?,
                    mailbox_id: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|_| AppError::state())
}

fn binding_for_address_in(
    connection: &Connection,
    address: &str,
) -> AppResult<Option<MailboxBinding>> {
    connection
        .query_row(
            "SELECT account_id, address, mailbox_id FROM account_mailboxes WHERE address = ?1",
            params![address],
            |row| {
                Ok(MailboxBinding {
                    account: row.get(0)?,
                    address: row.get(1)?,
                    mailbox_id: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|_| AppError::state())
}

fn initialize_or_validate_schema(connection: &Connection) -> AppResult<()> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| AppError::state())?;
    if version == 0 {
        let user_tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| AppError::state())?;
        if user_tables != 0 {
            return Err(AppError::new(
                ErrorCode::State,
                "插件状态 schema 未标记版本",
            ));
        }
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE account_mailboxes(
                    account_id TEXT PRIMARY KEY NOT NULL,
                    address TEXT NOT NULL UNIQUE,
                    mailbox_id INTEGER NOT NULL CHECK(mailbox_id > 0),
                    scan_offset INTEGER NOT NULL DEFAULT 0 CHECK(scan_offset >= 0)
                 );
                 CREATE TABLE mail_uid_map(
                    account_id TEXT NOT NULL,
                    remote_mail_id INTEGER NOT NULL CHECK(remote_mail_id > 0),
                    synthetic_uid INTEGER NOT NULL CHECK(synthetic_uid > 0),
                    tombstoned INTEGER NOT NULL DEFAULT 0 CHECK(tombstoned IN (0, 1)),
                    PRIMARY KEY(account_id, remote_mail_id),
                    UNIQUE(account_id, synthetic_uid),
                    FOREIGN KEY(account_id) REFERENCES account_mailboxes(account_id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=1;
                 COMMIT;",
            )
            .map_err(|_| AppError::state())?;
    } else if version != SCHEMA_VERSION {
        return Err(AppError::new(
            ErrorCode::State,
            "插件状态 schema 版本不受支持",
        ));
    }
    validate_schema_shape(connection)
}

fn validate_schema_shape(connection: &Connection) -> AppResult<()> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(|_| AppError::state())?;
    let names: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .map_err(|_| AppError::state())?
        .collect::<Result<_, _>>()
        .map_err(|_| AppError::state())?;
    if names != ["account_mailboxes", "mail_uid_map"] {
        return Err(AppError::new(ErrorCode::State, "插件状态 schema 结构无效"));
    }
    validate_columns(
        connection,
        "account_mailboxes",
        &[
            ("account_id", "TEXT", true, true),
            ("address", "TEXT", true, false),
            ("mailbox_id", "INTEGER", true, false),
            ("scan_offset", "INTEGER", true, false),
        ],
    )?;
    validate_columns(
        connection,
        "mail_uid_map",
        &[
            ("account_id", "TEXT", true, true),
            ("remote_mail_id", "INTEGER", true, true),
            ("synthetic_uid", "INTEGER", true, false),
            ("tombstoned", "INTEGER", true, false),
        ],
    )
}

fn validate_columns(
    connection: &Connection,
    table: &str,
    expected: &[(&str, &str, bool, bool)],
) -> AppResult<()> {
    let sql = format!("PRAGMA table_info({table})");
    let mut statement = connection.prepare(&sql).map_err(|_| AppError::state())?;
    let actual: Vec<(String, String, bool, bool)> = statement
        .query_map([], |row| {
            Ok((
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, i64>(5)? != 0,
            ))
        })
        .map_err(|_| AppError::state())?
        .collect::<Result<_, _>>()
        .map_err(|_| AppError::state())?;
    let expected: Vec<_> = expected
        .iter()
        .map(|(name, kind, not_null, primary)| {
            ((*name).to_owned(), (*kind).to_owned(), *not_null, *primary)
        })
        .collect();
    if actual != expected {
        return Err(AppError::new(
            ErrorCode::State,
            "插件状态 schema 列定义无效",
        ));
    }
    Ok(())
}

fn validate_account(account: &str) -> AppResult<()> {
    if account.is_empty()
        || account.len() > 128
        || account.chars().any(|character| character.is_control())
    {
        return Err(AppError::invalid_input());
    }
    Ok(())
}

pub fn validate_address(address: &str) -> AppResult<()> {
    canonicalize_address(address).map(|_| ())
}

pub fn canonicalize_address(address: &str) -> AppResult<String> {
    if address.is_empty()
        || address.len() > maild_client::MAX_MAILBOX_BYTES
        || address.trim() != address
        || address.chars().any(char::is_control)
    {
        return Err(AppError::invalid_input());
    }
    maild_client::validate_send_field(maild_client::SendField::From, address)
        .map_err(|_| AppError::invalid_input())?;
    let Some((local, domain)) = address.rsplit_once('@') else {
        return Err(AppError::invalid_input());
    };
    if local.is_empty()
        || !matches!(
            domain.to_ascii_lowercase().as_str(),
            "nodeseek.org" | "seek.li"
        )
    {
        return Err(AppError::invalid_input());
    }
    Ok(format!("{local}@{}", domain.to_ascii_lowercase()))
}

fn ensure_private_directory(path: &Path) -> AppResult<()> {
    let existed = path.exists();
    if !existed {
        fs::create_dir_all(path).map_err(|_| AppError::state())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|_| AppError::state())?;
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| AppError::state())?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::state());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.mode() & 0o777 != 0o700 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(AppError::state());
        }
    }
    Ok(())
}

fn ensure_state_file(path: &Path) -> AppResult<()> {
    if !path.exists() {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(path).map_err(|_| AppError::state())?;
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| AppError::state())?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::state());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.mode() & 0o777 != 0o600
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err(AppError::state());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_mapping_is_stable_across_reopen_and_remote_order() {
        let root = tempfile::tempdir().unwrap();
        let store = StateStore::at_root(root.path().join("state")).unwrap();
        store
            .import_mailbox("account-a", "box@seek.li", 41)
            .unwrap();
        store.assign_remote_ids("account-a", &[30, 10, 30]).unwrap();
        assert_eq!(
            store.pending("account-a", 0, 20).unwrap(),
            vec![
                PendingMail {
                    remote_mail_id: 10,
                    synthetic_uid: 1,
                },
                PendingMail {
                    remote_mail_id: 30,
                    synthetic_uid: 2,
                },
            ]
        );

        let reopened = StateStore::at_root(root.path().join("state")).unwrap();
        reopened
            .assign_remote_ids("account-a", &[20, 10, 30])
            .unwrap();
        assert_eq!(
            reopened.pending("account-a", 1, 20).unwrap(),
            vec![
                PendingMail {
                    remote_mail_id: 30,
                    synthetic_uid: 2,
                },
                PendingMail {
                    remote_mail_id: 20,
                    synthetic_uid: 3,
                },
            ]
        );
        assert_eq!(reopened.scan_offset("account-a").unwrap(), 0);
        reopened.set_scan_offset("account-a", 10).unwrap();
        reopened.mark_remote_missing("account-a", 30).unwrap();
        assert_eq!(
            reopened.pending("account-a", 1, 20).unwrap(),
            vec![PendingMail {
                remote_mail_id: 20,
                synthetic_uid: 3,
            }]
        );
        let reopened_again = StateStore::at_root(root.path().join("state")).unwrap();
        assert_eq!(reopened_again.scan_offset("account-a").unwrap(), 10);
    }

    #[test]
    fn uid_assignment_batch_is_bounded_and_empty_batch_is_safe() {
        let root = tempfile::tempdir().unwrap();
        let store = StateStore::at_root(root.path().join("state")).unwrap();
        store
            .import_mailbox("account-a", "box@seek.li", 41)
            .unwrap();
        store.assign_remote_ids("account-a", &[]).unwrap();
        let oversized: Vec<i64> = (1..=21).collect();
        assert_eq!(
            store
                .assign_remote_ids("account-a", &oversized)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidResponse
        );
        assert!(store.pending("account-a", 0, 20).unwrap().is_empty());
    }

    #[test]
    fn import_is_idempotent_but_refuses_rebinding() {
        let root = tempfile::tempdir().unwrap();
        let store = StateStore::at_root(root.path().join("state")).unwrap();
        let binding = store
            .import_mailbox("account-a", "box@seek.li", 41)
            .unwrap();
        assert_eq!(
            store
                .import_mailbox("account-a", "box@seek.li", 41)
                .unwrap(),
            binding
        );
        assert!(store
            .import_mailbox("account-a", "other@seek.li", 42)
            .is_err());
        assert!(store
            .import_mailbox("account-b", "box@seek.li", 41)
            .is_err());
    }

    #[test]
    fn unimport_is_exact_local_only_and_cascades_uid_state() {
        let root = tempfile::tempdir().unwrap();
        let store = StateStore::at_root(root.path().join("state")).unwrap();
        let binding = store
            .import_mailbox("account-old", "box@SEEK.LI", 41)
            .unwrap();
        store.assign_remote_ids("account-old", &[10, 20]).unwrap();
        store.set_scan_offset("account-old", 7).unwrap();

        assert_eq!(
            store.binding_for_address("box@seek.li").unwrap(),
            Some(binding.clone())
        );
        assert_eq!(
            store
                .unimport_mailbox("account-old", "other@seek.li")
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
        assert_eq!(store.binding("account-old").unwrap(), binding);
        assert_eq!(store.scan_offset("account-old").unwrap(), 7);
        assert_eq!(store.pending("account-old", 0, 20).unwrap().len(), 2);

        let removed = store
            .unimport_mailbox("account-old", "box@seek.li")
            .unwrap();
        assert_eq!(removed, binding);
        assert!(store.binding_for_address("box@seek.li").unwrap().is_none());
        assert_eq!(
            store.binding("account-old").unwrap_err().code(),
            ErrorCode::NotFound
        );
        assert_eq!(
            store
                .unimport_mailbox("account-old", "box@seek.li")
                .unwrap_err()
                .code(),
            ErrorCode::NotFound
        );

        store
            .import_mailbox("account-new", "box@seek.li", 41)
            .unwrap();
        store.assign_remote_ids("account-new", &[20]).unwrap();
        assert_eq!(
            store.pending("account-new", 0, 20).unwrap(),
            vec![PendingMail {
                remote_mail_id: 20,
                synthetic_uid: 1,
            }]
        );
    }

    #[test]
    fn address_domain_is_canonicalized_once_without_changing_local_part() {
        assert_eq!(
            canonicalize_address("CaseSensitive@SEEK.LI").unwrap(),
            "CaseSensitive@seek.li"
        );
        let root = tempfile::tempdir().unwrap();
        let store = StateStore::at_root(root.path().join("state")).unwrap();
        let binding = store
            .import_mailbox("account-a", "CaseSensitive@SEEK.LI", 41)
            .unwrap();
        assert_eq!(binding.address, "CaseSensitive@seek.li");
    }

    #[test]
    fn future_schema_and_cursor_ahead_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let state_root = root.path().join("state");
        let store = StateStore::at_root(state_root.clone()).unwrap();
        store
            .import_mailbox("account-a", "box@seek.li", 41)
            .unwrap();
        assert_eq!(store.pending("account-a", 0, 20).unwrap(), Vec::new());
        assert_eq!(
            store.pending("account-a", 1, 20).unwrap_err().code(),
            ErrorCode::State
        );
        let connection = Connection::open(store.path()).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        drop(connection);
        assert!(StateStore::at_root(state_root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn state_directory_and_database_require_exact_private_modes() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let state_root = root.path().join("state");
        let store = StateStore::at_root(state_root.clone()).unwrap();
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(StateStore::at_root(state_root.clone()).is_err());
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&state_root, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(StateStore::at_root(state_root).is_err());
    }
}
