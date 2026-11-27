//! Tauri 命令：`Bridge` 的薄封装。这里不做任何逻辑，好让 `bridge.rs` 在没有窗口的测试里
//! 对着真 daemon 跑。新增命令时同步改 `build.rs`、`capabilities/main.json` 与 `lib.rs` 的
//! `generate_handler!`。
//!
//! 多词参数的命令标 `rename_all = "snake_case"`：webview 侧的 DTO 与 Rust 同名（ADR-0031 决策 4）。

use tauri::State;

use crate::bridge::{
    UiContactSuggestion, UiDocument, UiError, UiFolder, UiFrontendInfo, UiLocale,
    UiLocalePreference, UiLocaleUpdate, UiMessage, UiPage, UiReadingPolicy, UiReadingPolicyPatch,
    UiSnapshot,
};
use crate::html::LinkKind;
use crate::mailbox::UiMailboxSyncOutcome;
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

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_search_contacts(
    state: State<'_, AppState>,
    account: String,
    query: Option<String>,
    limit: Option<u64>,
) -> Result<Vec<UiContactSuggestion>, UiError> {
    state
        .bridge
        .search_contacts(&account, query.as_deref(), limit)
        .await
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
        .ok_or_else(|| UiError::not_found("link_document_released", "这封邮件的文档已释放，请重新选择它。"))?;
    let link = document
        .links
        .get(index)
        .ok_or_else(|| UiError::not_found("link_missing", "链接不存在。"))?;
    if !matches!(link.kind, LinkKind::Http | LinkKind::Https) {
        return Err(UiError::invalid("link_scheme", "只有 http/https 链接会交给系统浏览器。"));
    }
    system::open_external(&link.href)
        .map_err(|error| UiError::failed("open_external", error, "browser_failed", "无法打开系统浏览器。"))
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
    .map_err(|error| UiError::failed("save_dialog", error, "save_dialog_failed", "无法打开保存面板。"))?;
    let Some(path) = receiver
        .await
        .map_err(|error| UiError::failed("save_dialog", error, "save_dialog_failed", "无法打开保存面板。"))?
    else {
        return Ok(None);
    };
    let bytes = saved.bytes;
    let written = tokio::task::spawn_blocking(move || system::write_atomically(&path, &bytes))
        .await
        .map_err(|error| UiError::failed("save_attachment", error, "save_failed", "保存未完成。"))?
        .map_err(|error| {
            UiError::failed(
                "save_attachment",
                error,
                "save_write_failed",
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

/// ADR-0048：壳手里当前的语言列表（M2：跟随系统时是本进程的语言）；不发请求。webview 启动时取
/// 一次，之后跟 `maild://locale`。
#[tauri::command]
pub(crate) fn ui_locale(state: State<'_, AppState>) -> UiLocale {
    state.locale.current()
}

/// 设置页的完整偏好；`effective` 换成壳的列表（M2）。
#[tauri::command]
pub(crate) async fn ui_locale_preference(
    state: State<'_, AppState>,
) -> Result<UiLocalePreference, UiError> {
    crate::shell_locale::for_settings(state.bridge.locale_preference().await).await
}

#[tauri::command]
pub(crate) async fn ui_set_locale_preference(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    update: UiLocaleUpdate,
) -> Result<UiLocalePreference, UiError> {
    let result = state.bridge.set_locale_preference(&update).await;
    // ADR-0048 决策 8：系统画的文字下次启动跟随（M1 镜像）；`effective` 不变时没有推送，这里直接
    // 同步，并把应答的 `effective` 换成壳的列表（M2），webview 按应答立即切换。
    crate::shell_locale::after_set(&app, result).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_proxy_config(
    state: State<'_, AppState>,
) -> Result<crate::settings::UiProxyConfig, UiError> {
    state.bridge.proxy_config().await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_set_proxy_config(
    state: State<'_, AppState>,
    update: crate::settings::UiProxyConfigUpdate,
) -> Result<crate::settings::UiProxyConfig, UiError> {
    state.bridge.set_proxy_config(update).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_test_proxy(
    state: State<'_, AppState>,
    request: crate::settings::UiTestProxyRequest,
) -> Result<crate::settings::UiTestProxyResult, UiError> {
    state.bridge.test_proxy(request).await
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
    locale: Option<String>,
) -> Result<crate::compose::UiDraft, UiError> {
    state
        .bridge
        .reply_draft(&account, &folder, &uid, &mode, locale.as_deref())
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
    .map_err(|e| UiError::failed("pick", e, "file_picker_failed", "无法打开文件选择器。"))?;
    let paths = rx
        .await
        .map_err(|e| UiError::failed("pick", e, "file_picker_failed", "无法打开文件选择器。"))?
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
    let effective=state.locale.current().effective.unwrap_or_default();
    let title=package_folder_title(maild_l10n::negotiate(&effective));
    let (tx,rx)=tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || { let _=tx.send(rfd::FileDialog::new().set_title(title).pick_folder()); })
        .map_err(|_|UiError::invalid("folder_picker_failed", "无法打开目录选择器。"))?;
    let Some(path)=rx.await.map_err(|_|UiError::invalid("folder_picker_no_result", "目录选择器未返回。"))? else {return Ok(None)};
    state.packages.select(&state.bridge,&path,&kind).await.map(Some)
}

/// 扩展包目录选择器的标题跟随壳的界面语言（ADR-0048）。
fn package_folder_title(locale: maild_l10n::Locale) -> String {
    let product = maild_brand::PRODUCT_NAME;
    match locale {
        maild_l10n::Locale::ZhHans => format!("选择完整的 {product} 扩展包目录"),
        maild_l10n::Locale::En => format!("Choose the complete {product} extension package folder"),
    }
}

// —— 日常收信与登录向导（T4 契约：`packages/featherpost-studio/src/bridge/tauri.ts`）——

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_sync_mailbox(
    state: State<'_, AppState>,
    account: Option<String>,
) -> Result<(), UiError> {
    state.bridge.sync_mailbox(account).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_sync_mailbox_scope(
    state: State<'_, AppState>,
    account: String,
    folder: Option<String>,
) -> Result<UiMailboxSyncOutcome, UiError> {
    state.bridge.sync_mailbox_scope(account, folder).await
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn ui_mailbox_sync_status(
    state: State<'_, AppState>,
    account: String,
    sync_id: u64,
) -> Result<bool, UiError> {
    state.bridge.mailbox_sync_status(&account, sync_id).await
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
        .map_err(|error| UiError::failed("ui_open_external_url", error, "external_link_failed", "无法打开这个链接。"))
}

/// 不依赖 daemon 直接打开原生修复与后台管理窗口（离线或独立前端兜底）。
#[tauri::command]
pub(crate) fn ui_open_native_recovery(state: State<'_, AppState>) -> Result<(), UiError> {
    if state.custom_connection || maild_client::NativeAppLayout::has_custom_connection() {
        return Err(UiError::invalid(
            "recovery_unavailable",
            &format!(
                "修复入口未能打开，请从完整 {} 打开修复与后台管理。",
                maild_brand::APP_BUNDLE_NAME
            ),
        ));
    }
    crate::standalone_handoff::open_native_recovery_directly().map_err(|error| {
        eprintln!("[maild-ui] 离线修复入口打开失败: {error}");
        UiError::invalid(
            "recovery_unavailable",
            &format!(
                "修复入口未能打开，请从完整 {} 打开修复与后台管理。",
                maild_brand::APP_BUNDLE_NAME
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::package_folder_title;
    use maild_l10n::Locale;

    #[test]
    fn package_folder_picker_title_follows_the_shell_language() {
        assert_eq!(package_folder_title(Locale::ZhHans), "选择完整的 Featherpost 扩展包目录");
        assert_eq!(
            package_folder_title(Locale::En),
            "Choose the complete Featherpost extension package folder"
        );
    }
}
