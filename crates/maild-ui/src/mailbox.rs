//! Typed mailbox commands. Queue payloads and account credentials never reach JS.
use crate::bridge::{UiMessage, UiPage, UiSummary};
use crate::{Bridge, UiError};
use maild_client::{MessageResult, MessageSummariesResult, ProfilesResult};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::future::Future;
use tokio::time::Instant;

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
    /// ADR-0048 决策 3：摘要 detail 的稳定 code（`discarded` / `rejected` / `server_rejected` /
    /// `unconfirmed`）；老 daemon 没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_code: Option<String>,
    /// ADR-0048 L6：服务器命令失败时服务器给的原文（daemon 已有界脱敏），作为数据显示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_detail: Option<String>,
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
                return Err(UiError::invalid("search_with_filter", "搜索不接受列表筛选。"));
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
                return Err(UiError::invalid("search_query_required", "请使用搜索查询。"));
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
            return Err(UiError::invalid("selection_size", "请选择 1 至 500 封同一文件夹的邮件。"));
        }
        let mut params = json!({"account":r.account,"folder":r.folder,"uids":r.uids});
        let method = match r.action.as_str() {
            "mark" => {
                let flag = r
                    .flag
                    .filter(|f| ["seen", "flagged"].contains(&f.as_str()))
                    .ok_or_else(|| UiError::invalid("flag_invalid", "标记无效。"))?;
                let value = r.value.ok_or_else(|| UiError::invalid("flag_value_missing", "缺少标记值。"))?;
                params["flag"] = json!(flag);
                params["value"] = json!(value);
                "markMessages"
            }
            "move" => {
                let target = r
                    .target
                    .filter(|t| !t.is_empty() && t != &r.folder)
                    .ok_or_else(|| UiError::invalid("move_target_required", "请选择其他文件夹。"))?;
                params["target"] = json!(target);
                "moveMessages"
            }
            "delete" => "deleteMessages",
            _ => return Err(UiError::invalid("mailbox_action_unknown", "未知邮箱操作。")),
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
            return Err(UiError::invalid("resolution_invalid", "请选择有效的恢复动作。"));
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

    pub async fn sync_mailbox_scope(
        &self,
        account: String,
        folder: Option<String>,
    ) -> Result<UiMailboxSyncOutcome, UiError> {
        #[derive(serde::Deserialize)]
        struct Started {
            sync_id: u64,
        }
        let deadline = Instant::now() + maild_client::REQUEST_TIMEOUT;
        let Started { sync_id } = self
            .compose_call(
                "syncMessages",
                json!({ "account": account, "folder": folder, "track": true }),
            )
            .await?;
        wait_for_mailbox_sync(deadline, sync_id, || self.mailbox_sync_status(&account, sync_id))
            .await
    }

    pub async fn mailbox_sync_status(&self, account: &str, sync_id: u64) -> Result<bool, UiError> {
        #[derive(serde::Deserialize)]
        struct Status {
            done: bool,
        }
        let status: Status = self
            .compose_call("syncMessages", json!({ "account": account, "sync_id": sync_id }))
            .await?;
        Ok(status.done)
    }

    /// 在服务器上检索本地缓存窗口之外的历史（`searchMessages` 的 `scope: "server"` 分支）。
    /// 结果只是摘要、不落库；要读全文请接着调 [`Bridge::fetch_server_message`]。
    pub async fn server_search(
        &self,
        account: String,
        folder: String,
        query: String,
    ) -> Result<Vec<UiSummary>, UiError> {
        self.mailbox_server_search(account, folder, query, None, None)
            .await
    }

    /// 带可选 `since` / `until`（按 `sort_at` 秒级边界过滤）的服务器历史检索。
    pub async fn mailbox_server_search(
        &self,
        account: String,
        folder: String,
        query: String,
        since: Option<u64>,
        until: Option<u64>,
    ) -> Result<Vec<UiSummary>, UiError> {
        let query = query.trim().to_string();
        if account.is_empty() || folder.is_empty() || query.is_empty() {
            return Err(UiError::invalid("server_search_required", "请选择账户与文件夹，并输入搜索内容。"));
        }
        if query.len() > MAX_QUERY_BYTES || query.chars().any(char::is_control) {
            return Err(UiError::invalid("server_search_invalid", "搜索内容过长或包含无效字符。"));
        }
        let mut params =
            json!({ "account": account, "folder": folder, "query": query, "scope": "server" });
        for (key, value) in [("since", since), ("until", until)] {
            if let Some(value) = value {
                params[key] = json!(value);
            }
        }
        let result: maild_client::SearchResult =
            self.compose_call("searchMessages", params).await?;
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
            return Err(UiError::invalid("message_id_invalid", "邮件标识无效。"));
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

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiMailboxSyncOutcome {
    Complete,
    Background { sync_id: u64 },
}

async fn wait_for_mailbox_sync<F>(
    deadline: Instant,
    sync_id: u64,
    mut status: impl FnMut() -> F,
) -> Result<UiMailboxSyncOutcome, UiError>
where
    F: Future<Output = Result<bool, UiError>>,
{
    match tokio::time::timeout_at(deadline, async {
        loop {
            if status().await? {
                return Ok(UiMailboxSyncOutcome::Complete);
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Ok(UiMailboxSyncOutcome::Background { sync_id }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn expired_sync_wait_returns_background() {
        let outcome = wait_for_mailbox_sync(Instant::now(), 42, || async { Ok(false) })
            .await
            .unwrap();
        assert_eq!(outcome, UiMailboxSyncOutcome::Background { sync_id: 42 });
        assert_eq!(
            serde_json::to_value(outcome).unwrap(),
            json!({ "background": { "sync_id": 42 } })
        );
    }

    #[tokio::test]
    async fn sync_wait_preserves_poll_errors() {
        let error = wait_for_mailbox_sync(Instant::now() + Duration::from_secs(1), 42, || async {
            Err(UiError::failed(
                "syncMessages",
                "LIST rejected",
                "failed",
                "同步失败",
            ))
        })
        .await
        .unwrap_err();
        assert_eq!(error.message, "同步失败");
    }
}
