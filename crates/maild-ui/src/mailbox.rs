//! Typed mailbox commands. Queue payloads and account credentials never reach JS.
use crate::bridge::{UiMessage, UiPage, UiSummary};
use crate::{Bridge, UiError};
use maild_client::{MessageResult, MessageSummariesResult, ProfilesResult};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxQuery {
    #[serde(default)]
    pub search: bool,
    pub account: Option<String>,
    pub folder: Option<String>,
    pub role: Option<String>,
    pub seen: Option<bool>,
    pub flagged: Option<bool>,
    pub query: Option<String>,
    pub from: Option<String>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub has_attachments: Option<bool>,
}
#[derive(Serialize)]
pub struct UiProfile {
    pub id: String,
    pub display_name: String,
    pub address: String,
    pub enabled: bool,
}
#[derive(Serialize)]
pub struct UiThread {
    pub thread_id: i64,
    pub messages: Vec<UiSummary>,
    pub total: i64,
}
#[derive(Deserialize, Serialize)]
pub struct UiIndex {
    pub pending: i64,
    pub done: bool,
}
#[derive(Deserialize, Serialize)]
pub struct UiOperation {
    pub subject: Option<String>,
    pub id: i64,
    pub account: String,
    pub folder: String,
    pub uid: String,
    pub kind: String,
    pub state: String,
    pub attempts: i64,
    pub created_at: u64,
    pub updated_at: u64,
    pub detail: String,
    pub needs_decision: bool,
    pub target: Option<String>,
    pub flag: Option<String>,
    pub value: Option<bool>,
}
#[derive(Deserialize, Serialize)]
pub struct UiActivity {
    pub ops: Vec<UiOperation>,
    pub has_more: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailboxWrite {
    pub action: String,
    pub account: String,
    pub folder: String,
    pub uids: Vec<String>,
    pub flag: Option<String>,
    pub value: Option<bool>,
    pub target: Option<String>,
}
#[derive(Deserialize, Serialize)]
pub struct UiReceipt {
    pub queued: usize,
    pub mode: Option<String>,
    pub queued_uids: Vec<String>,
    pub unchanged_uids: Vec<String>,
    pub missing_uids: Vec<String>,
}

impl Bridge {
    pub async fn mailbox_profiles(&self) -> Result<Vec<UiProfile>, UiError> {
        let r: ProfilesResult = self.compose_call("listAccountProfiles", json!({})).await?;
        Ok(r.profiles
            .into_iter()
            .filter(|p| p.archived_at.is_none())
            .map(|p| UiProfile {
                id: p.id,
                display_name: p.display_name,
                address: p.address,
                enabled: p.enabled,
            })
            .collect())
    }
    pub async fn mailbox_query(&self, q: MailboxQuery, offset: u64) -> Result<UiPage, UiError> {
        let mut params = json!({"limit":50,"offset":offset});
        for (key, value) in [("account", q.account), ("folder", q.folder)] {
            if let Some(v) = value {
                params[key] = json!(v);
            }
        }
        let method = if q.search {
            if q.role.is_some() || q.seen.is_some() || q.flagged.is_some() {
                return Err(UiError::invalid("搜索不接受列表筛选。"));
            }
            for (k, v) in [("query", q.query), ("from", q.from)] {
                if let Some(v) = v {
                    params[k] = json!(v);
                }
            }
            for (k, v) in [("since", q.since), ("until", q.until)] {
                if let Some(v) = v {
                    params[k] = json!(v);
                }
            }
            if let Some(v) = q.has_attachments {
                params["hasAttachments"] = json!(v);
            }
            "searchMessages"
        } else {
            if q.query.is_some()
                || q.from.is_some()
                || q.since.is_some()
                || q.until.is_some()
                || q.has_attachments.is_some()
            {
                return Err(UiError::invalid("请使用搜索查询。"));
            }
            if let Some(v) = q.role {
                params["role"] = json!(v);
            }
            for (k, v) in [("seen", q.seen), ("flagged", q.flagged)] {
                if let Some(v) = v {
                    params[k] = json!(v);
                }
            }
            "listMessageSummaries"
        };
        let r: MessageSummariesResult = self.compose_call(method, params).await?;
        Ok(UiPage {
            messages: r.messages.into_iter().map(UiSummary::from).collect(),
            total: r.total,
        })
    }
    pub async fn mailbox_thread(
        &self,
        account: String,
        folder: String,
        uid: String,
        offset: u64,
    ) -> Result<UiThread, UiError> {
        #[derive(Deserialize)]
        struct Reply {
            #[serde(rename = "threadId")]
            id: i64,
            messages: Vec<maild_client::MessageSummary>,
            total: i64,
        }
        let r: Reply = self
            .compose_call(
                "getThread",
                json!({"account":account,"folder":folder,"uid":uid,"limit":30,"offset":offset}),
            )
            .await?;
        Ok(UiThread {
            thread_id: r.id,
            messages: r.messages.into_iter().map(UiSummary::from).collect(),
            total: r.total,
        })
    }
    pub async fn mailbox_locate(
        &self,
        account: String,
        folder: String,
        uid: String,
    ) -> Result<UiMessage, UiError> {
        let r: MessageResult = self
            .compose_call(
                "getMessage",
                json!({"account":account,"folder":folder,"uid":uid}),
            )
            .await?;
        Ok(r.into())
    }
    pub async fn mailbox_index(&self) -> Result<UiIndex, UiError> {
        self.compose_call("getSearchIndexStatus", json!({})).await
    }
    pub async fn mailbox_write(&self, r: MailboxWrite) -> Result<UiReceipt, UiError> {
        if r.account.is_empty()
            || r.folder.is_empty()
            || r.uids.is_empty()
            || r.uids.len() > 500
            || r.uids.iter().any(|s| s.is_empty())
        {
            return Err(UiError::invalid("请选择 1 至 500 封同一文件夹的邮件。"));
        }
        let mut params = json!({"account":r.account,"folder":r.folder,"uids":r.uids});
        let method = match r.action.as_str() {
            "mark" => {
                let flag = r
                    .flag
                    .filter(|f| ["seen", "flagged"].contains(&f.as_str()))
                    .ok_or_else(|| UiError::invalid("标记无效。"))?;
                let value = r.value.ok_or_else(|| UiError::invalid("缺少标记值。"))?;
                params["flag"] = json!(flag);
                params["value"] = json!(value);
                "markMessages"
            }
            "move" => {
                let target = r
                    .target
                    .filter(|t| !t.is_empty() && t != &r.folder)
                    .ok_or_else(|| UiError::invalid("请选择其他文件夹。"))?;
                params["target"] = json!(target);
                "moveMessages"
            }
            "delete" => "deleteMessages",
            _ => return Err(UiError::invalid("未知邮箱操作。")),
        };
        self.compose_call(method, params).await
    }
    pub async fn mailbox_activity(
        &self,
        account: Option<String>,
        before_id: Option<i64>,
    ) -> Result<UiActivity, UiError> {
        self.compose_call("listPendingOps",json!({"account":account,"include_settled":true,"summary_only":true,"limit":40,"before_id":before_id})).await
    }
    pub async fn mailbox_resolve(
        &self,
        id: i64,
        resolution: String,
    ) -> Result<UiOperation, UiError> {
        if id <= 0 || !["retry", "discard"].contains(&resolution.as_str()) {
            return Err(UiError::invalid("请选择有效的恢复动作。"));
        }
        #[derive(Deserialize)]
        struct Reply {
            op: UiOperation,
        }
        let r: Reply = self
            .compose_call(
                "resolvePendingOp",
                json!({"id":id,"resolution":resolution,"summary_only":true}),
            )
            .await?;
        Ok(r.op)
    }
}

/// 服务器检索一次最多回多少条给 webview（与 daemon 的 `MAX_SERVER_SEARCH_HITS` 同量级，
/// 这里再兜一道，免得旧/异常后台把一大包摘要塞进 webview）。
const MAX_SERVER_HITS: usize = 200;
const MAX_QUERY_BYTES: usize = 512;

impl Bridge {
    /// 「立即同步」（ADR-0041 决策 4）：敲醒后台收信循环。只敲门，不等同步完成——
    /// 新邮件经状态推送与列表刷新体现。`account = None` = 全部启用账户。
    pub async fn sync_mailbox(&self, account: Option<String>) -> Result<(), UiError> {
        let params = match account.filter(|account| !account.is_empty()) {
            Some(account) => json!({ "account": account }),
            None => json!({}),
        };
        let _: maild_client::SyncMessagesResult =
            self.compose_call("syncMessages", params).await?;
        Ok(())
    }

    /// 在服务器上检索本地缓存窗口之外的历史（`searchMessages` 的 `scope: "server"` 分支）。
    /// 结果只是摘要、不落库；要读全文请接着调 [`Bridge::fetch_server_message`]。
    pub async fn server_search(
        &self,
        account: String,
        folder: String,
        query: String,
    ) -> Result<Vec<UiSummary>, UiError> {
        let query = query.trim().to_string();
        if account.is_empty() || folder.is_empty() || query.is_empty() {
            return Err(UiError::invalid("请选择账户与文件夹，并输入搜索内容。"));
        }
        if query.len() > MAX_QUERY_BYTES || query.chars().any(char::is_control) {
            return Err(UiError::invalid("搜索内容过长或包含无效字符。"));
        }
        let result: maild_client::SearchResult = self
            .compose_call(
                "searchMessages",
                json!({ "account": account, "folder": folder, "query": query, "scope": "server" }),
            )
            .await?;
        Ok(result
            .messages
            .into_iter()
            .take(MAX_SERVER_HITS)
            .map(UiSummary::from)
            .collect())
    }

    /// 把一封服务器历史邮件拉回本地库并返回安全详情（不含 `body_html`；HTML 仍只走
    /// `ui_prepare_html` 的隔离文档——拉回后本地已有，隔离文档通道照常可用）。
    /// 服务器上也没有这封信 → `None`。
    pub async fn fetch_server_message(
        &self,
        account: String,
        folder: String,
        uid: String,
    ) -> Result<Option<UiMessage>, UiError> {
        if account.is_empty()
            || folder.is_empty()
            || uid.parse::<u32>().ok().filter(|uid| *uid > 0).is_none()
        {
            return Err(UiError::invalid("邮件标识无效。"));
        }
        match self
            .compose_call::<MessageResult>(
                "fetchServerMessage",
                json!({ "account": account, "folder": folder, "uid": uid }),
            )
            .await
        {
            Ok(result) => Ok(Some(result.into())),
            Err(error) if error.code == crate::UiErrorCode::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}
