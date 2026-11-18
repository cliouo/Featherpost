use super::*;

impl Store {
    /// A versioned durable fence. No network I/O happens until this transaction commits.
    pub fn claim_draft_send(
        &self,
        id: &str,
        version: u64,
        now: u64,
    ) -> rusqlite::Result<DraftWrite> {
        self.transition_draft_send(id, version, &["editing", "failed"], "sending", None, now)
    }
    pub fn settle_draft_send(
        &self,
        id: &str,
        version: u64,
        state: &str,
        archived: Option<bool>,
        now: u64,
    ) -> rusqlite::Result<DraftWrite> {
        assert!(matches!(state, "sent" | "unknown" | "failed"));
        self.transition_draft_send(id, version, &["sending"], state, archived, now)
    }
    pub fn resolve_draft_send(
        &self,
        id: &str,
        version: u64,
        now: u64,
    ) -> rusqlite::Result<DraftWrite> {
        self.transition_draft_send(id, version, &["unknown"], "failed", None, now)
    }
    fn transition_draft_send(
        &self,
        id: &str,
        version: u64,
        allowed: &[&str],
        state: &str,
        archived: Option<bool>,
        now: u64,
    ) -> rusqlite::Result<DraftWrite> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut draft) = load_draft(&tx, id)? else {
            return Ok(DraftWrite::NotFound);
        };
        if draft.version != version {
            return Ok(DraftWrite::Conflict {
                version: draft.version,
            });
        }
        if !allowed.contains(&draft.send_state.as_str()) {
            return Ok(DraftWrite::SendLocked);
        }
        tx.execute("UPDATE drafts SET send_state=?2, sent_archive=?3, version=version+1, updated_at=?4 WHERE id=?1", params![id, state, archived, now])?;
        draft.send_state = state.into();
        draft.sent_archive = archived;
        draft.version += 1;
        draft.updated_at = now;
        draft.attachments = read_draft_attachments(&tx, id)?;
        tx.commit()?;
        Ok(DraftWrite::Saved(Box::new(draft)))
    }
    pub fn record_draft_archive(&self, id: &str, queued: bool) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE drafts SET sent_archive=?2 WHERE id=?1 AND send_state='sent'",
            params![id, queued],
        )?;
        Ok(())
    }
}
