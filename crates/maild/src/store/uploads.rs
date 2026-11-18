use super::*;
use crate::uploads::{UploadError, UploadStatus, MESSAGE_BYTES};

pub(super) struct StoredFile {
    pub filename: String,
    pub mime_type: String,
    pub content: Vec<u8>,
    pub owner: String,
}
impl StoredFile {
    pub fn reference(&self, account: &str, id: &str) -> DraftAttachmentRef {
        DraftAttachmentRef {
            upload_id: Some(id.into()),
            account: account.into(),
            filename: self.filename.clone(),
            mime_type: self.mime_type.clone(),
            size: self.content.len() as u64,
            stored: true,
            ..Default::default()
        }
    }
}
pub(super) fn load_file(
    conn: &Connection,
    draft_id: &str,
    upload_id: &str,
) -> rusqlite::Result<Option<StoredFile>> {
    // Validate length in SQLite before allocating a Vec, including after backup restore.
    conn.query_row(
        "SELECT filename, mime_type, upload_owner, size,
                CASE WHEN typeof(content)='blob' AND length(content)=size AND size <= ?3
                     THEN content ELSE NULL END
         FROM draft_attachments WHERE draft_id=?1 AND upload_id=?2 AND content IS NOT NULL",
        params![draft_id, upload_id, crate::uploads::FILE_BYTES],
        |row| {
            let filename: String = row.get(0)?;
            let mime_type: String = row.get(1)?;
            let size: u64 = row.get(3)?;
            crate::uploads::validate_metadata(&filename, &mime_type, size).map_err(|_| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    Type::Text,
                    Box::new(std::io::Error::other("invalid saved attachment metadata")),
                )
            })?;
            Ok(StoredFile {
                filename,
                mime_type,
                owner: row.get(2)?,
                content: row.get(4)?,
            })
        },
    )
    .optional()
}
pub(super) fn insert_file(
    conn: &Connection,
    draft_id: &str,
    account: &str,
    position: usize,
    id: &str,
    file: &StoredFile,
) -> rusqlite::Result<()> {
    conn.execute("INSERT INTO draft_attachments (draft_id, position, account, folder, uid, part_index, filename, mime_type, size, upload_id, upload_owner, content) VALUES (?1,?2,?3,'','',0,?4,?5,?6,?7,?8,?9)",
        params![draft_id, position as i64, account, file.filename, file.mime_type, file.content.len() as i64, id, file.owner, file.content])?;
    Ok(())
}
impl Store {
    pub fn begin_attachment_upload(
        &self,
        owner: &str,
        draft_id: &str,
        filename: &str,
        mime_type: &str,
        size: u64,
    ) -> Result<UploadStatus, UploadError> {
        // All paths that require both locks take uploads first, then the connection.
        let mut uploads = self.uploads.lock().unwrap();
        let conn = self.conn.lock().unwrap();
        if load_draft(&conn, draft_id)?.is_none() {
            return Err(UploadError::Draft(DraftWrite::NotFound));
        }
        uploads.begin(&self.db_path, owner, draft_id, filename, mime_type, size)
    }
    pub fn write_attachment_upload(
        &self,
        owner: &str,
        id: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<UploadStatus, UploadError> {
        self.uploads.lock().unwrap().write(id, owner, offset, data)
    }
    pub fn cancel_attachment_upload(&self, owner: &str, id: &str) -> Result<(), UploadError> {
        self.uploads.lock().unwrap().cancel(id, owner)
    }
    pub fn finish_attachment_upload(
        &self,
        owner: &str,
        id: &str,
        version: u64,
        now: u64,
    ) -> Result<Draft, UploadError> {
        let mut uploads = self.uploads.lock().unwrap();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // A dropped response can be retried even after daemon restart. Owner is not an RPC field.
        let completed: Option<String> = tx.query_row("SELECT draft_id FROM draft_attachments WHERE upload_id=?1 AND upload_owner=?2 AND content IS NOT NULL", params![id, owner], |r| r.get(0)).optional()?;
        if let Some(draft_id) = completed {
            let mut draft =
                load_draft(&tx, &draft_id)?.ok_or(UploadError::Draft(DraftWrite::NotFound))?;
            draft.attachments = read_draft_attachments(&tx, &draft_id)?;
            uploads.remove(id);
            return Ok(draft);
        }
        let upload = uploads.get(id, owner)?;
        let mut draft =
            load_draft(&tx, &upload.draft_id)?.ok_or(UploadError::Draft(DraftWrite::NotFound))?;
        if draft.version != version {
            return Err(UploadError::Draft(DraftWrite::Conflict {
                version: draft.version,
            }));
        }
        if !matches!(draft.send_state.as_str(), "editing" | "failed") {
            return Err(UploadError::Draft(DraftWrite::SendLocked));
        }
        draft.attachments = read_draft_attachments(&tx, &draft.id)?;
        if let Some(position) = draft
            .attachments
            .iter()
            .position(|a| a.upload_id.is_some() && !a.stored)
        {
            return Err(UploadError::Draft(DraftWrite::AttachmentNotStored {
                position,
            }));
        }
        if draft.attachments.len() >= crate::drafts::MAX_DRAFT_ATTACHMENTS {
            return Err(UploadError::Limit(
                "attachments",
                crate::drafts::MAX_DRAFT_ATTACHMENTS as u64,
            ));
        }
        if draft
            .attachments
            .iter()
            .try_fold(upload.size, |a, f| a.checked_add(f.size))
            .is_none_or(|n| n > MESSAGE_BYTES)
        {
            return Err(UploadError::Limit("attachment_total_bytes", MESSAGE_BYTES));
        }
        let file = StoredFile {
            content: upload.bytes()?,
            filename: upload.filename.clone(),
            mime_type: upload.mime_type.clone(),
            owner: owner.into(),
        };
        insert_file(
            &tx,
            &draft.id,
            &draft.account,
            draft.attachments.len(),
            id,
            &file,
        )?;
        draft.attachments.push(file.reference(&draft.account, id));
        draft.version += 1;
        draft.updated_at = now;
        tx.execute(
            "UPDATE drafts SET version=?2, updated_at=?3 WHERE id=?1",
            params![draft.id, draft.version as i64, now as i64],
        )?;
        tx.commit()?;
        uploads.remove(id);
        Ok(draft)
    }
    pub fn draft_file_content(
        &self,
        draft_id: &str,
        upload_id: &str,
    ) -> rusqlite::Result<Option<crate::compose::OutgoingAttachment>> {
        let conn = self.conn.lock().unwrap();
        Ok(
            load_file(&conn, draft_id, upload_id)?.map(|f| crate::compose::OutgoingAttachment {
                filename: f.filename,
                mime_type: f.mime_type,
                content: f.content,
            }),
        )
    }
}

pub(super) fn spawn_cleanup(ctx: &Context, store: &Arc<Store>) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let weak = Arc::downgrade(&store.uploads);
    let task = runtime.spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let Some(uploads) = weak.upgrade() else {
                return;
            };
            // Never block an async worker behind a large finish/write; next tick retries.
            if let Ok(mut guard) = uploads.try_lock() {
                guard.sweep(std::time::Instant::now());
            };
        }
    });
    ctx.effect_async(async move {
        task.abort();
        let _ = task.await;
    });
}
