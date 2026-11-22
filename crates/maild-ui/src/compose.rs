//! Typed compose bridge. Draft HTML and account credentials never enter the host DOM.
use crate::{Bridge, UiError};
use maild_client::{
    AddressesResult, DraftAttachmentView, DraftListResult, DraftPatchInput, DraftResult, DraftView,
    MessageDraft, PrepareReplyResult, ProfilesResult,
};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize)]
pub struct UiDraft {
    pub id: String,
    pub account: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub has_html: bool,
    pub attachments: Vec<DraftAttachmentView>,
    pub version: u64,
    pub updated_at: u64,
    pub send_state: String,
    pub sent_archive: Option<bool>,
}
impl From<DraftView> for UiDraft {
    fn from(d: DraftView) -> Self {
        Self {
            id: d.id,
            account: d.account,
            from: d.from,
            to: d.to,
            cc: d.cc,
            bcc: d.bcc,
            subject: d.subject,
            body: d.body,
            has_html: !d.body_html.is_empty(),
            attachments: d.attachments,
            version: d.version,
            updated_at: d.updated_at,
            send_state: d.send_state,
            sent_archive: d.sent_archive,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Sender {
    pub account: String,
    pub address: String,
    pub label: String,
    pub available: bool,
    pub simple: bool,
}
impl Bridge {
    pub async fn senders(&self) -> Result<Vec<Sender>, UiError> {
        let profiles: ProfilesResult = self.compose_call("listAccountProfiles", json!({})).await?;
        let mut senders = Vec::new();
        for p in profiles
            .profiles
            .into_iter()
            .filter(|p| p.archived_at.is_none())
        {
            let available = p.enabled && p.outgoing.is_some();
            let simple = p.outgoing.as_ref().is_some_and(|o| o.kind == "plugin");
            senders.push(Sender {
                account: p.id.clone(),
                address: p.address.clone(),
                label: if p.display_name.is_empty() {
                    p.address.clone()
                } else {
                    p.display_name.clone()
                },
                available,
                simple,
            });
            if p.source == maild_client::AccountSource::Legacy {
                continue;
            }
            let mut offset = 0;
            loop {
                let page: AddressesResult = self
                    .compose_call(
                        "listAddresses",
                        json!({"account":p.id,"limit":500,"offset":offset}),
                    )
                    .await?;
                let count = page.addresses.len();
                for a in page
                    .addresses
                    .into_iter()
                    .filter(|a| a.state == "active" && a.address != p.address)
                {
                    senders.push(Sender {
                        account: p.id.clone(),
                        label: if a.label.is_empty() {
                            "别名".into()
                        } else {
                            a.label
                        },
                        address: a.address,
                        available,
                        simple,
                    });
                }
                offset += count;
                if count == 0 || offset >= page.total {
                    break;
                }
            }
        }
        Ok(senders)
    }
    pub async fn drafts(&self, offset: u64) -> Result<DraftListResult, UiError> {
        // Eight maximum-sized summaries stay below the 1 MiB wire budget.
        self.compose_call("listDrafts", json!({"limit":8,"offset":offset}))
            .await
    }
    pub async fn draft(&self, id: &str) -> Result<UiDraft, UiError> {
        self.draft_action("getDraft", json!({"id":id})).await
    }
    pub async fn new_draft(
        &self,
        account: &str,
        patch: DraftPatchInput,
    ) -> Result<UiDraft, UiError> {
        let mut p = serde_json::to_value(patch).map_err(|_| UiError::invalid("draft_fields_invalid", "草稿字段无效。"))?;
        p["account"] = json!(account);
        self.draft_action("createDraft", p).await
    }
    pub async fn save_draft(
        &self,
        id: &str,
        version: u64,
        patch: DraftPatchInput,
    ) -> Result<UiDraft, UiError> {
        let mut p = serde_json::to_value(patch).map_err(|_| UiError::invalid("draft_fields_invalid", "草稿字段无效。"))?;
        p["id"] = json!(id);
        p["base_version"] = json!(version);
        self.draft_action("updateDraft", p).await
    }
    pub async fn reply_draft(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
        mode: &str,
        locale: Option<&str>,
    ) -> Result<UiDraft, UiError> {
        let params = json!({"account":account,"folder":folder,"uid":uid,"mode":mode});
        // ADR-0048 决策 7：引用块语言首版跟随界面语言。早于 L4 的 daemon 拒绝未知参数（-32602），
        // 去掉 `locale` 重试一次，得到与今天相同的中文引用块。
        let reply: PrepareReplyResult = match locale {
            Some(locale) => {
                let mut localized = params.clone();
                localized["locale"] = json!(locale);
                match self.compose_call("prepareReply", localized).await {
                    Err(error) if error.code == crate::bridge::UiErrorCode::Invalid => {
                        self.compose_call("prepareReply", params).await?
                    }
                    result => result?,
                }
            }
            None => self.compose_call("prepareReply", params).await?,
        };
        let MessageDraft {
            to,
            cc,
            subject,
            body,
            in_reply_to,
            references,
            attachments,
        } = reply.draft;
        let attachments = attachments
            .into_iter()
            .map(|a| maild_client::DraftAttachmentInput {
                account: Some(a.account),
                folder: a.folder,
                uid: a.uid,
                part_index: a.part_index,
                ..Default::default()
            })
            .collect();
        self.new_draft(
            account,
            DraftPatchInput {
                to: Some(to),
                cc: Some(cc),
                subject: Some(subject),
                body: Some(body),
                in_reply_to: Some(in_reply_to),
                references: Some(references),
                attachments: Some(attachments),
                ..Default::default()
            },
        )
        .await
    }
    pub async fn deliver_draft(&self, id: &str, version: u64) -> Result<UiDraft, UiError> {
        self.draft_action("sendDraft", json!({"id":id,"base_version":version}))
            .await
    }
    pub async fn resolve_send(&self, id: &str, version: u64) -> Result<UiDraft, UiError> {
        self.draft_action("resolveDraftSend", json!({"id":id,"base_version":version}))
            .await
    }
    pub async fn discard_draft(&self, id: &str, version: u64) -> Result<(), UiError> {
        self.draft_action("deleteDraft", json!({"id":id,"base_version":version}))
            .await?;
        Ok(())
    }
    pub async fn finish_upload(&self, id: &str, version: u64) -> Result<UiDraft, UiError> {
        self.draft_action(
            "finishAttachmentUpload",
            json!({"upload_id":id,"base_version":version}),
        )
        .await
    }
    pub async fn cancel_upload(&self, id: &str) -> Result<(), UiError> {
        let _: Value = self
            .compose_call("cancelAttachmentUpload", json!({"upload_id":id}))
            .await?;
        Ok(())
    }
    pub(crate) async fn draft_action(
        &self,
        method: &str,
        params: Value,
    ) -> Result<UiDraft, UiError> {
        let r: DraftResult = self.compose_call(method, params).await?;
        Ok(r.draft.into())
    }
    pub(crate) async fn compose_call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, UiError> {
        self.guard(method)?;
        self.handle()
            .request_typed(method, params)
            .await
            .map_err(|e| UiError::from_client(method, e))
    }
}
