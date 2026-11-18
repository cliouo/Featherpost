//! Tauri 命令：`Bridge` 的薄封装。这里不做任何逻辑，好让 `bridge.rs` 在没有窗口的测试里
//! 对着真 daemon 跑。新增命令时同步改 `build.rs`、`capabilities/main.json` 与 `lib.rs` 的
//! `generate_handler!`。
//!
//! 多词参数的命令标 `rename_all = "snake_case"`：webview 侧的 DTO 与 Rust 同名（ADR-0031 决策 4）。

use tauri::State;

use crate::bridge::{
    UiDocument, UiError, UiFolder, UiFrontendInfo, UiMessage, UiPage, UiReadingPolicy,
    UiReadingPolicyPatch, UiSnapshot,
};
use crate::html::LinkKind;
use crate::{system, AppState};

#[tauri::command]
pub(crate) fn ui_snapshot(state: State<'_, AppState>) -> UiSnapshot {
    state.bridge.snapshot()
}

#[tauri::command]
pub(crate) async fn ui_list_folders(
    state: State<'_, AppState>,
    account: String,
) -> Result<Vec<UiFolder>, UiError> {
    state.bridge.list_folders(&account).await
}

#[tauri::command]
pub(crate) async fn ui_list_messages(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    offset: u64,
    limit: u64,
) -> Result<UiPage, UiError> {
    state
        .bridge
        .list_messages(&account, &folder, offset, limit)
        .await
}

#[tauri::command]
pub(crate) async fn ui_get_message(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: String,
) -> Result<UiMessage, UiError> {
    state.bridge.get_message(&account, &folder, &uid).await
}

#[tauri::command]
pub(crate) async fn ui_refresh(state: State<'_, AppState>) -> Result<(), UiError> {
    state.bridge.refresh().await
}

#[tauri::command]
pub(crate) async fn ui_frontend_info(
    state: State<'_, AppState>,
) -> Result<UiFrontendInfo, UiError> {
    state.bridge.frontend_info().await
}

#[tauri::command]
pub(crate) async fn ui_claim_intent(
    state: State<'_, AppState>,
) -> Result<Option<maild_client::OpenIntent>, UiError> {
    state.bridge.claim_intent().await
}

// —— ADR-0033：HTML 原位阅读 ——

/// 准备隔离文档。`allow_remote` 由 UI 按内核结论与本次会话的「加载本封」决定（决策 5）。
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_prepare_html(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: String,
    allow_remote: bool,
) -> Result<UiDocument, UiError> {
    state
        .bridge
        .prepare_document(
            &state.documents,
            &state.base,
            &account,
            &folder,
            &uid,
            allow_remote,
        )
        .await
}

/// 切信即释放：令牌失效后 `/doc/`、`/part/` 都是 404。
#[tauri::command]
pub(crate) fn ui_release_html(state: State<'_, AppState>, token: String) -> bool {
    state.documents.release(&token)
}

/// 只把链接表里 `http`/`https` 的那一项交给系统浏览器；webview 传不进 URL 字符串（决策 6）。
#[tauri::command]
pub(crate) fn ui_open_link(
    state: State<'_, AppState>,
    token: String,
    index: usize,
) -> Result<(), UiError> {
    let document = state
        .documents
        .get(&token)
        .ok_or_else(|| UiError::not_found("这封邮件的文档已释放，请重新选择它。"))?;
    let link = document
        .links
        .get(index)
        .ok_or_else(|| UiError::not_found("链接不存在。"))?;
    if !matches!(link.kind, LinkKind::Http | LinkKind::Https) {
        return Err(UiError::invalid("只有 http/https 链接会交给系统浏览器。"));
    }
    system::open_external(&link.href)
        .map_err(|error| UiError::failed("open_external", error, "无法打开系统浏览器。"))
}

/// 保存附件：先取字节，再在主线程弹系统保存面板（rfd 的 macOS 后端要求），最后原子写入。
/// 用户取消返回 `Ok(None)`。
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_save_attachment(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: String,
    part_index: u32,
) -> Result<Option<String>, UiError> {
    let saved = state
        .bridge
        .read_attachment(&account, &folder, &uid, part_index)
        .await?;
    let suggested = system::safe_filename(&saved.attachment.filename, part_index);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let picked = rfd::FileDialog::new().set_file_name(&suggested).save_file();
        let _ = sender.send(picked);
    })
    .map_err(|error| UiError::failed("save_dialog", error, "无法打开保存面板。"))?;
    let Some(path) = receiver
        .await
        .map_err(|error| UiError::failed("save_dialog", error, "无法打开保存面板。"))?
    else {
        return Ok(None);
    };
    let bytes = saved.bytes;
    let written = tokio::task::spawn_blocking(move || system::write_atomically(&path, &bytes))
        .await
        .map_err(|error| UiError::failed("save_attachment", error, "保存未完成。"))?
        .map_err(|error| {
            UiError::failed(
                "save_attachment",
                error,
                "无法写入所选位置，已有文件未被改动。",
            )
        })?;
    Ok(Some(written.display().to_string()))
}

#[tauri::command]
pub(crate) async fn ui_reading_policy(
    state: State<'_, AppState>,
) -> Result<UiReadingPolicy, UiError> {
    state.bridge.reading_policy().await
}

#[tauri::command]
pub(crate) async fn ui_set_reading_policy(
    state: State<'_, AppState>,
    patch: UiReadingPolicyPatch,
) -> Result<UiReadingPolicy, UiError> {
    state.bridge.set_reading_policy(&patch).await
}

// ADR-0036: compose commands. No generic RPC or local-path entry point is exposed.
#[tauri::command]
pub(crate) async fn ui_senders(
    state: State<'_, AppState>,
) -> Result<Vec<crate::compose::Sender>, UiError> {
    state.bridge.senders().await
}
#[tauri::command]
pub(crate) async fn ui_drafts(
    state: State<'_, AppState>,
    offset: u64,
) -> Result<maild_client::DraftListResult, UiError> {
    state.bridge.drafts(offset).await
}
#[tauri::command]
pub(crate) async fn ui_draft(
    state: State<'_, AppState>,
    id: String,
) -> Result<crate::compose::UiDraft, UiError> {
    state.bridge.draft(&id).await
}
#[tauri::command]
pub(crate) async fn ui_new_draft(
    state: State<'_, AppState>,
    account: String,
    patch: maild_client::DraftPatchInput,
) -> Result<crate::compose::UiDraft, UiError> {
    state.bridge.new_draft(&account, patch).await
}
#[tauri::command]
pub(crate) async fn ui_save_draft(
    state: State<'_, AppState>,
    id: String,
    version: u64,
    patch: maild_client::DraftPatchInput,
) -> Result<crate::compose::UiDraft, UiError> {
    state.bridge.save_draft(&id, version, patch).await
}
#[tauri::command]
pub(crate) async fn ui_reply_draft(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: String,
    mode: String,
) -> Result<crate::compose::UiDraft, UiError> {
    state
        .bridge
        .reply_draft(&account, &folder, &uid, &mode)
        .await
}
#[tauri::command]
pub(crate) async fn ui_send_draft(
    state: State<'_, AppState>,
    id: String,
    version: u64,
) -> Result<crate::compose::UiDraft, UiError> {
    state.bridge.deliver_draft(&id, version).await
}
#[tauri::command]
pub(crate) async fn ui_resolve_send(
    state: State<'_, AppState>,
    id: String,
    version: u64,
) -> Result<crate::compose::UiDraft, UiError> {
    state.bridge.resolve_send(&id, version).await
}
#[tauri::command]
pub(crate) async fn ui_discard_draft(
    state: State<'_, AppState>,
    id: String,
    version: u64,
) -> Result<(), UiError> {
    state.bridge.discard_draft(&id, version).await
}
#[tauri::command]
pub(crate) async fn ui_pick_files(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<Result<crate::compose_files::PickedFile, UiError>>, UiError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(rfd::FileDialog::new().pick_files());
    })
    .map_err(|e| UiError::failed("pick", e, "无法打开文件选择器。"))?;
    let paths = rx
        .await
        .map_err(|e| UiError::failed("pick", e, "无法打开文件选择器。"))?
        .unwrap_or_default();
    Ok(paths.iter().map(|p| state.files.select(p)).collect())
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_upload_file(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    draft_id: String,
    token: String,
) -> Result<String, UiError> {
    use tauri::Emitter;
    state
        .files
        .upload(&state.bridge, &draft_id, &token, move |p| {
            let _ = app.emit("maild://file-progress", p);
        })
        .await
}
#[tauri::command]
pub(crate) fn ui_release_file(state: State<'_, AppState>, token: String) {
    state.files.release(&token)
}
#[tauri::command]
pub(crate) async fn ui_finish_upload(
    state: State<'_, AppState>,
    id: String,
    version: u64,
) -> Result<crate::compose::UiDraft, UiError> {
    state.bridge.finish_upload(&id, version).await
}
#[tauri::command]
pub(crate) async fn ui_cancel_upload(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), UiError> {
    state.bridge.cancel_upload(&id).await
}
#[tauri::command]
pub(crate) fn ui_finish_close(app: tauri::AppHandle) {
    // ADR-0046：前端已刷盘草稿——进退出流水线（撤窗 → `Accessory` → 等 runloop 提交 → 退出）。
    crate::retire_and_exit(app);
}

#[tauri::command]
pub(crate) fn ui_compose_ready(state: State<'_, AppState>) {
    state
        .guard_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_mailbox_profiles(
    state: State<'_, AppState>,
) -> Result<Vec<crate::mailbox::UiProfile>, UiError> {
    state.bridge.mailbox_profiles().await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_mailbox_query(
    state: State<'_, AppState>,
    query: crate::mailbox::MailboxQuery,
    offset: u64,
) -> Result<crate::bridge::UiPage, UiError> {
    state.bridge.mailbox_query(query, offset).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_mailbox_thread(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: String,
    offset: u64,
) -> Result<crate::mailbox::UiThread, UiError> {
    state
        .bridge
        .mailbox_thread(account, folder, uid, offset)
        .await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_mailbox_locate(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: String,
) -> Result<crate::bridge::UiMessage, UiError> {
    state.bridge.mailbox_locate(account, folder, uid).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_mailbox_index(
    state: State<'_, AppState>,
) -> Result<crate::mailbox::UiIndex, UiError> {
    state.bridge.mailbox_index().await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_mailbox_write(
    state: State<'_, AppState>,
    request: crate::mailbox::MailboxWrite,
) -> Result<crate::mailbox::UiReceipt, UiError> {
    state.bridge.mailbox_write(request).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_mailbox_activity(
    state: State<'_, AppState>,
    account: Option<String>,
    before_id: Option<i64>,
) -> Result<crate::mailbox::UiActivity, UiError> {
    state.bridge.mailbox_activity(account, before_id).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_mailbox_resolve(
    state: State<'_, AppState>,
    id: i64,
    resolution: String,
) -> Result<crate::mailbox::UiOperation, UiError> {
    state.bridge.mailbox_resolve(id, resolution).await
}

// ADR-0038: typed management surface.
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_profiles(state: State<'_, AppState>, ) -> Result<Vec<maild_client::AccountProfileView>, UiError> { state.bridge.settings_profiles().await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_probe(state: State<'_, AppState>, request: crate::settings::AccountEdit,) -> Result<maild_client::ProbeReportView, UiError> { state.bridge.settings_probe(request).await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_save_account(state: State<'_, AppState>, request: crate::settings::AccountEdit,) -> Result<maild_client::AccountProfileView, UiError> { state.bridge.settings_save_account(request).await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_account_action(state: State<'_, AppState>, request: crate::settings::AccountAction,) -> Result<(), UiError> { state.bridge.settings_account_action(request).await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_rules(state: State<'_, AppState>, patch: Option<crate::settings::RulesConfig>,) -> Result<crate::settings::UiRules, UiError> { state.bridge.settings_rules(patch).await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_notification(state: State<'_, AppState>, request: bool,) -> Result<maild_client::NotificationStatus, UiError> { state.bridge.settings_notification(request).await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_addresses(state: State<'_, AppState>, account: String, offset: u64,) -> Result<crate::settings::AddressPage, UiError> { state.bridge.settings_addresses(&account,offset).await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_address_action(state: State<'_, AppState>, request: crate::settings::AddressAction,) -> Result<(), UiError> { state.bridge.settings_address_action(request).await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_packages(state: State<'_, AppState>, ) -> Result<crate::settings::UiPackages, UiError> { state.bridge.settings_packages().await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_package_action(state: State<'_, AppState>, request: crate::settings::PackageAction,) -> Result<(), UiError> { state.bridge.settings_package_action(request).await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_rollback_preview(state: State<'_, AppState>, kind: String, id: String,) -> Result<maild_client::PackagePreview, UiError> { state.bridge.settings_rollback_preview(&kind,&id).await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_rollback(state: State<'_, AppState>, preview: maild_client::PackagePreview,) -> Result<(), UiError> { state.bridge.settings_rollback(preview).await }
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_install(state: State<'_, AppState>, token: String,) -> Result<maild_client::CommitPluginInstallResult, UiError> { state.packages.install(&state.bridge,&token).await }
#[tauri::command]
pub(crate) fn ui_settings_cancel_package(state: State<'_,AppState>,token: String) { state.packages.cancel(&token); }
#[tauri::command]
pub(crate) async fn ui_settings_pick_package(app: tauri::AppHandle,state: State<'_,AppState>,kind: String) -> Result<Option<crate::settings_packages::PackageChoice>,UiError> {
    crate::settings::check_kind(&kind)?;
    let (tx,rx)=tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || { let _=tx.send(rfd::FileDialog::new().set_title(format!("选择完整的 {} 扩展包目录", maild_brand::PRODUCT_NAME)).pick_folder()); })
        .map_err(|_|UiError::invalid("无法打开目录选择器。"))?;
    let Some(path)=rx.await.map_err(|_|UiError::invalid("目录选择器未返回。"))? else {return Ok(None)};
    state.packages.select(&state.bridge,&path,&kind).await.map(Some)
}

// —— 日常收信与登录向导（T4 契约：`packages/maild-ui/src/bridge/tauri.ts`）——

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_sync_mailbox(
    state: State<'_, AppState>,
    account: Option<String>,
) -> Result<(), UiError> {
    state.bridge.sync_mailbox(account).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_server_search(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    query: String,
    since: Option<u64>,
    until: Option<u64>,
) -> Result<Vec<crate::bridge::UiSummary>, UiError> {
    state
        .bridge
        .mailbox_server_search(account, folder, query, since, until)
        .await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_fetch_server_message(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: String,
) -> Result<Option<UiMessage>, UiError> {
    state.bridge.fetch_server_message(account, folder, uid).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_settings_discover_provider(
    state: State<'_, AppState>,
    address: String,
) -> Result<Option<crate::settings::DiscoveredServers>, UiError> {
    state.bridge.settings_discover_provider(address).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_discover_provider(
    state: State<'_, AppState>,
    input: String,
    allow_network: Option<bool>,
) -> Result<maild_client::ProviderDiscovery, UiError> {
    state
        .bridge
        .discover_provider(input, allow_network.unwrap_or(false))
        .await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn ui_open_external_url(url: String) -> Result<(), UiError> {
    system::open_web_url(&url)
        .map_err(|error| UiError::failed("ui_open_external_url", error, "无法打开这个链接。"))
}
