use super::*;

#[derive(Default)]
pub struct SummaryFilter {
    pub account: Option<String>,
    pub folder: Option<String>,
    pub role: Option<String>,
    pub seen: Option<bool>,
    pub flagged: Option<bool>,
}

impl Store {
    pub fn summary_page(
        &self,
        query: &SummaryFilter,
        limit: usize,
        offset: usize,
    ) -> rusqlite::Result<(Vec<MessageSummary>, i64)> {
        let conn = self.conn.lock().unwrap();
        let (mut filter, mut values) =
            message_filter(query.account.as_deref(), query.folder.as_deref());
        if let Some(role) = &query.role {
            values.push(role.clone());
            filter.push_str(&format!(" AND f.role = ?{}", values.len()));
        }
        for (column, value) in [("seen", query.seen), ("flagged", query.flagged)] {
            if let Some(value) = value {
                filter.push_str(&format!(" AND m.{column} = {}", i32::from(value)));
            }
        }
        let mut bound: Vec<&dyn rusqlite::ToSql> =
            values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let total = conn.query_row(
            &format!("SELECT COUNT(*) {MESSAGE_FROM} {filter}"),
            bound.as_slice(),
            |r| r.get(0),
        )?;
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} {MESSAGE_FROM} {filter} {MESSAGE_ORDER} LIMIT ?{} OFFSET ?{}",
            bound.len() + 1,
            bound.len() + 2
        );
        let limit = limit as i64;
        let offset = offset as i64;
        bound.extend([&limit as &dyn rusqlite::ToSql, &offset]);
        let messages = conn
            .prepare(&sql)?
            .query_map(bound.as_slice(), decode_summary_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok((messages, total))
    }

    pub fn thread_page(
        &self,
        account: &str,
        thread_id: i64,
        limit: usize,
        offset: usize,
    ) -> rusqlite::Result<(Vec<MessageSummary>, i64)> {
        let conn = self.conn.lock().unwrap();
        let filter = format!("{MESSAGE_FROM} WHERE {VISIBLE} AND m.account=?1 AND m.thread_id=?2");
        let total = conn.query_row(
            &format!("SELECT COUNT(*) {filter}"),
            params![account, thread_id],
            |r| r.get(0),
        )?;
        let messages = conn.prepare(&format!("SELECT {SUMMARY_COLUMNS} {filter} ORDER BY m.sort_at ASC,m.id ASC LIMIT ?3 OFFSET ?4"))?
            .query_map(params![account,thread_id,limit as i64,offset as i64],decode_summary_row)?.collect::<rusqlite::Result<_>>()?;
        Ok((messages, total))
    }

    /// A bounded header lookup also finds locally hidden move sources, without MIME/body reads.
    pub fn pending_op_subject(&self, id: i64) -> rusqlite::Result<Option<String>> {
        self.conn.lock().unwrap().query_row(
            "SELECT substr(m.subject,1,512) FROM pending_ops o JOIN messages m ON m.folder_id=o.folder_id AND m.uid=o.uid AND m.account=o.account WHERE o.id=?1",
            params![id],|row|row.get(0)).optional()
    }

    pub fn pending_op_page(
        &self,
        account: Option<&str>,
        only_open: bool,
        limit: usize,
        before_id: Option<i64>,
        summary: bool,
    ) -> rusqlite::Result<Vec<PendingOp>> {
        let conn = self.conn.lock().unwrap();
        let mut clauses = vec!["1=1".to_string()];
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(account) = account {
            values.push(account.to_owned().into());
            clauses.push(format!("o.account=?{}", values.len()));
        }
        if let Some(id) = before_id {
            values.push(id.into());
            clauses.push(format!("o.id<?{}", values.len()));
        }
        if only_open {
            clauses.push("o.state IN ('pending','in_flight','unknown')".into());
        }
        // Never materialize an APPEND's potentially multi-megabyte MIME in summary queries.
        let columns = if summary {
            PENDING_OP_COLUMNS
                .replace(
                    "o.payload",
                    "CASE WHEN o.kind='append' THEN '' ELSE o.payload END",
                )
                .replace("o.detail", "substr(o.detail,1,512)")
        } else {
            PENDING_OP_COLUMNS.into()
        };
        values.push((limit as i64).into());
        let rows = conn
            .prepare(&format!(
                "SELECT {columns} {PENDING_OP_FROM} WHERE {} ORDER BY o.id DESC LIMIT ?{}",
                clauses.join(" AND "),
                values.len()
            ))?
            .query_map(
                rusqlite::params_from_iter(values.iter()),
                decode_pending_op_row,
            )?
            .collect();
        rows
    }
}

#[derive(Default)]
pub struct WriteReceipt {
    pub queued_uids: Vec<String>,
    pub unchanged_uids: Vec<String>,
    pub missing_uids: Vec<String>,
}
