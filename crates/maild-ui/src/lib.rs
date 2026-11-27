//! 默认前端（产品名见 `maild_brand::PRODUCT_NAME`）的 Tauri 后端（ADR-0024 决策 4，ADR-0031，ADR-0033）。
//!
//! 结构：`bridge`（对 daemon 的唯一出口，无窗口可测）→ `commands`（薄封装）→ 本文件的 `run`
//! （窗口、导航否决、自定义协议、事件泵、退出）。`html` 与 `documents` 是 HTML 原位阅读的
//! 重写器与令牌文档表，同样不依赖事件循环。webview 侧不知道 endpoint、token、协议或 DTO 校验。

pub mod bridge;
mod commands;
pub mod compose;
pub mod compose_files;
pub mod documents;
pub mod html;
pub mod instance_lock;
#[cfg(target_os = "macos")]
mod macos_app_languages;
#[cfg(target_os = "macos")]
mod macos_locale;
#[cfg(target_os = "macos")]
mod macos_window;
pub mod mailbox;
pub mod remote_image;
pub mod system;
pub mod settings;
pub mod settings_packages;
mod shell_locale;
pub mod standalone_handoff;
pub mod translation;

/// Packaging contract (ADR-0039): only the `embedded` form serves its assets from the binary.
/// A default-form build would load `devUrl` from a local port, so `build-bundle.sh` and
/// `verify-bundle.sh` refuse any bundled `maild-ui` whose every slice lacks this marker.
#[cfg(feature = "embedded")]
#[used]
static EMBEDDED_CONTRACT: &str = maild_brand::ui_embedded_contract!();

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{Emitter as _, Manager as _, Url, WindowEvent};

use bridge::UiLink;
pub use bridge::{
    Bridge, UiContactSuggestion, UiError, UiErrorCode, UiLocale, UiSnapshot, FRONTEND_ID,
    INTENT_EVENT, LINK_EVENT, LOCALE_EVENT, PROXY_EVENT, STATE_EVENT,
};
use documents::{
    document_response_for_query, navigation_verdict, not_found, part_response,
    remote_image_response, route, DocumentStore, ProtocolBase, Route, Verdict, SCHEME,
};
pub use translation::{
    TranslationManager, TranslationStoragePaths, TRANSLATION_PROGRESS_EVENT,
    TRANSLATION_PROVIDERS_EVENT,
};

/// 启动参数（由 `main.rs` 解析并透传真实活动配置目录与缓存命名空间）。
#[derive(Clone, Debug)]
pub struct LaunchOptions {
    /// 启动时是否传入了自定义连接参数（`--config/--url/--socket/--pipe`）；为真时禁用平台离线修复入口。
    pub custom_connection: bool,
    /// 真实活动配置父目录与翻译缓存命名空间（绝不静默回退默认 `AppPaths::discover()`）。
    pub translation_paths: TranslationStoragePaths,
}

pub struct AppState {
    pub bridge: Arc<Bridge>,
    pub documents: Arc<DocumentStore>,
    pub remote_images: Arc<remote_image::RemoteImageFetcher>,
    pub translation: Arc<TranslationManager>,
    pub base: ProtocolBase,
    pub files: compose_files::FileSelections,
    pub packages: settings_packages::PackageSelections,
    pub close_allowed: AtomicBool,
    pub guard_ready: AtomicBool,
    /// ADR-0046：退出流水线（撤窗 → `Accessory` → `app.exit(0)`）已经启动；防重入。
    pub exiting: AtomicBool,
    /// ADR-0048 决策 8（M2）：壳的语言列表，`ui_locale`、`maild://locale` 与原生菜单都读它。
    pub locale: shell_locale::ShellLocale,
    /// 启动时是否传入了自定义连接参数（`--config/--url/--socket/--pipe`）；为真时禁用平台离线修复入口。
    pub custom_connection: bool,
}

/// 起 Tauri 事件循环。必须在主线程调用；`tauri::async_runtime` 需已由调用方绑定到
/// 持有连接 actor 的 runtime（见 `main.rs`）。
pub fn run(bridge: Arc<Bridge>) -> Result<(), tauri::Error> {
    run_with_options(bridge, false)
}

/// 同 [`run`]，额外记录启动时是否使用了自定义连接参数。
pub fn run_with_options(bridge: Arc<Bridge>, custom_connection: bool) -> Result<(), tauri::Error> {
    let translation_paths = TranslationStoragePaths::resolve(None, None, None, None)
        .map_err(|err| tauri::Error::Io(std::io::Error::other(err)))?;
    run_with_launch_options(
        bridge,
        LaunchOptions {
            custom_connection,
            translation_paths,
        },
    )
}

/// 同 [`run`]，接收由 `main.rs` 从产品配置解析得到的真实 [`LaunchOptions`]。
pub fn run_with_launch_options(
    bridge: Arc<Bridge>,
    launch: LaunchOptions,
) -> Result<(), tauri::Error> {
    let documents = Arc::new(DocumentStore::new());
    let remote_images = Arc::new(remote_image::RemoteImageFetcher::new());
    let translation = Arc::new(
        TranslationManager::new(launch.translation_paths, bridge.clone())
            .map_err(|err| tauri::Error::Io(std::io::Error::other(err.to_string())))?,
    );
    let base = ProtocolBase::for_platform();
    let for_setup = bridge.clone();
    let for_proxy = bridge.clone();
    let for_focus = bridge.clone();
    let for_exit = bridge.clone();
    let remote_for_state = remote_images.clone();
    let remote_for_proxy = remote_images.clone();
    let translation_for_state = translation.clone();
    let translation_for_proxy = translation.clone();
    let documents_for_window = documents.clone();
    let base_for_window = base.clone();
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.menu(macos_locale::menu);
    let app = builder
        .manage(AppState {
            bridge,
            documents,
            remote_images,
            translation,
            base,
            files: compose_files::FileSelections::default(),
            packages: settings_packages::PackageSelections::default(),
            close_allowed: AtomicBool::new(false),
            guard_ready: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            locale: shell_locale::ShellLocale::default(),
            custom_connection: launch.custom_connection,
        })
        // ADR-0033 决策 4：文档与 part 只经令牌可达。处理器在 tokio 上异步跑——part 要经认证连接
        // 向 daemon 取字节；Phase 3 远程图片经 `RemoteImageFetcher`（走 `maild-proxy`）抓取。
        .register_asynchronous_uri_scheme_protocol(SCHEME, |context, request, responder| {
            let app = context.app_handle().clone();
            let path = request.uri().path().to_string();
            let query = request.uri().query().map(str::to_string);
            // 诊断：只记路由形状与令牌前 8 位，不记 URL 全文。
            eprintln!(
                "[maild-ui] 协议请求 {} {}",
                request.method(),
                path.chars().take(14).collect::<String>()
            );
            tauri::async_runtime::spawn(async move {
                let state = app.state::<AppState>();
                let response = match route(&path) {
                    Route::Doc(token) => match state.documents.get(&token) {
                        Some(document) => document_response_for_query(&document, query.as_deref()),
                        None => not_found(),
                    },
                    Route::Part(token, part_index) => {
                        match state
                            .bridge
                            .serve_part(&state.documents, &token, part_index)
                            .await
                        {
                            Ok((mime, bytes)) => part_response(&mime, bytes),
                            // 原因已在桥里进 stderr；对文档来说只是「图没有」。
                            Err(_) => not_found(),
                        }
                    }
                    Route::Remote(token, index) => {
                        match state
                            .remote_images
                            .serve_remote(&state.documents, &token, index)
                            .await
                        {
                            Ok(image) => remote_image_response(&image.content_type, image.bytes),
                            Err(_) => not_found(),
                        }
                    }
                    Route::Unknown => not_found(),
                };
                responder.respond(response);
            });
        })
        .invoke_handler(tauri::generate_handler![
            commands::ui_settings_profiles,
            commands::ui_settings_probe,
            commands::ui_settings_save_account,
            commands::ui_settings_account_action,
            commands::ui_settings_rules,
            commands::ui_settings_notification,
            commands::ui_settings_addresses,
            commands::ui_settings_address_action,
            commands::ui_settings_packages,
            commands::ui_settings_package_action,
            commands::ui_settings_rollback_preview,
            commands::ui_settings_rollback,
            commands::ui_settings_install,
            commands::ui_settings_cancel_package,
            commands::ui_settings_pick_package,
            commands::ui_senders,
            commands::ui_drafts,
            commands::ui_draft,
            commands::ui_new_draft,
            commands::ui_save_draft,
            commands::ui_reply_draft,
            commands::ui_send_draft,
            commands::ui_resolve_send,
            commands::ui_discard_draft,
            commands::ui_pick_files,
            commands::ui_upload_file,
            commands::ui_release_file,
            commands::ui_finish_upload,
            commands::ui_cancel_upload,
            commands::ui_compose_ready,
            commands::ui_finish_close,
            commands::ui_snapshot,
            commands::ui_mailbox_profiles,
            commands::ui_mailbox_query,
            commands::ui_mailbox_thread,
            commands::ui_mailbox_locate,
            commands::ui_mailbox_index,
            commands::ui_mailbox_write,
            commands::ui_mailbox_activity,
            commands::ui_mailbox_resolve,
            commands::ui_list_folders,
            commands::ui_list_messages,
            commands::ui_get_message,
            commands::ui_search_contacts,
            commands::ui_refresh,
            commands::ui_frontend_info,
            commands::ui_claim_intent,
            commands::ui_prepare_html,
            commands::ui_release_html,
            commands::ui_open_link,
            commands::ui_save_attachment,
            commands::ui_reading_policy,
            commands::ui_set_reading_policy,
            commands::ui_locale,
            commands::ui_locale_preference,
            commands::ui_set_locale_preference,
            commands::ui_proxy_config,
            commands::ui_set_proxy_config,
            commands::ui_test_proxy,
            commands::ui_sync_mailbox,
            commands::ui_sync_mailbox_scope,
            commands::ui_mailbox_sync_status,
            commands::ui_server_search,
            commands::ui_fetch_server_message,
            commands::ui_settings_discover_provider,
            commands::ui_discover_provider,
            commands::ui_open_external_url,
            commands::ui_open_native_recovery,
            commands::ui_translation_get_preference,
            commands::ui_translation_save_preference,
            commands::ui_translation_list_providers,
            commands::ui_translation_test_provider,
            commands::ui_translation_detect_language,
            commands::ui_translation_translate_html,
            commands::ui_translation_translate_plain,
            commands::ui_translation_cancel,
            commands::ui_translation_cache_stats,
            commands::ui_translation_clear_cache,
        ])
        .setup(move |app| {
            // ADR-0045：`LSUIElement` 包以 `Accessory` 起步；窗口可见期间是 `Regular`
            // （系统菜单栏 `⌘C/⌘V`、`⌘Tab` 照常）。setup 就在主线程，直接切。
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Regular);
            create_main_window(app, documents_for_window, base_for_window)?;
            #[cfg(target_os = "macos")]
            macos_window::activate_app();
            // ADR-0046：Dock 右键「退出」与 `⌘Q` 走 `[NSApp terminate:]`，tao 的 delegate 不拦它；
            // 补上 `applicationShouldTerminate:`，统一路由进下面的退出流水线。
            #[cfg(target_os = "macos")]
            macos_window::install_terminate_handler(app.handle().clone());
            spawn_state_pump(
                app.handle().clone(),
                for_setup,
                remote_for_state,
                translation_for_state,
            );
            spawn_proxy_pump(
                app.handle().clone(),
                for_proxy,
                remote_for_proxy,
                translation_for_proxy,
            );
            Ok(())
        })
        .on_window_event(move |window, event| {
            // ADR-0045/0046：关闭 = 退出 UI 进程。窗口从不由系统直接销毁，一律走退出流水线：
            // 先撤下窗口与 Dock 图标再退出，WebView 内存全部释放；后台只剩 Agent 与菜单栏 `✉`。
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                request_close_and_exit(window.app_handle(), CloseTrigger::User);
            }
            if let WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
                let selected: Vec<_> = paths
                    .iter()
                    .map(|path| window.state::<AppState>().files.select(path))
                    .collect();
                let _ = window.emit("maild://files-picked", selected);
            }
            // 通知点击、Dock 再次打开都落到同一个主窗口（UX-SPEC §11）：聚焦即去 claim。
            if matches!(event, WindowEvent::Focused(true)) && window.label() == "main" {
                spawn_intent_claim(window.app_handle().clone(), for_focus.clone());
            }
        })
        .build(tauri::generate_context!())?;
    app.run(move |handle, event| {
        // ADR-0046：尚未走完退出流水线的退出请求一律拦下改走流水线；流水线自己的 `app.exit(0)`
        // 到这里时 `close_allowed` 已置位，直接放行。
        if let tauri::RunEvent::ExitRequested { api, .. } = &event {
            if !handle
                .state::<AppState>()
                .close_allowed
                .load(Ordering::SeqCst)
            {
                api.prevent_exit();
                request_close_and_exit(handle, CloseTrigger::User);
            }
        }
        // ADR-0044：Dock 点击、通知点击或菜单栏「写邮件 / 设置」经 NSWorkspace 重新打开已在运行的
        // 主 app bundle 时，窗口可能早已在前台，`Focused(true)` 不会再触发；Reopen 统一带回窗口并领意图。
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = &event {
            show_and_activate_main_window(handle);
            let bridge = handle.state::<AppState>().bridge.clone();
            spawn_intent_claim(handle.clone(), bridge);
        }
        if let tauri::RunEvent::Exit = event {
            // ADR-0046 最后一道防线：无论经哪条路走到这里，进程死前都已是 `Accessory`，
            // `launchservicesd` 不会把它记成 `exited-with-subordinates`。`Exit` 在主线程派发。
            #[cfg(target_os = "macos")]
            macos_window::demote_to_accessory_on_main_thread(None);
            // 关闭窗口 = 退出 UI 进程；daemon 不受影响——壳没有任何停止后台的调用面（决策 9）。
            let bridge = for_exit.clone();
            tauri::async_runtime::block_on(async move { bridge.shutdown().await });
        }
    });
    Ok(())
}

/// ADR-0045：把主窗口带回前台。macOS 上先切回 `Regular`（Dock 图标与系统菜单栏随窗口回来），
/// 再 `unminimize → show → set_focus` 并显式激活应用——黄灯 / `⌘M` 原生最小化在 Dock 的窗口
/// 也由此还原。任意线程可调：实际动作排到主线程。
pub(crate) fn show_and_activate_main_window(app: &tauri::AppHandle) {
    // ADR-0046：退出流水线已启动就不再把窗口与 Dock 图标请回来。
    if app.state::<AppState>().exiting.load(Ordering::SeqCst) {
        return;
    }
    let handle = app.clone();
    let scheduled = app.run_on_main_thread(move || {
        #[cfg(target_os = "macos")]
        let _ = handle.set_activation_policy(tauri::ActivationPolicy::Regular);
        if let Some(window) = handle.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
        #[cfg(target_os = "macos")]
        macos_window::activate_app();
    });
    if let Err(error) = scheduled {
        eprintln!("[maild-ui] 无法把主窗口带回前台: {error}");
    }
}

/// 后台停机（`agent/stopping`）时，前端收到 `maild://close-request` 后回调 `ui_finish_close` 的
/// 宽限期。后台已在离场、草稿反正存不进去，超时即直接走 `retire_and_exit`。
const CLOSE_REQUEST_GRACE: Duration = Duration::from_millis(500);

/// 撤窗并切到 `Accessory` 之后、真正退出之前留给主线程 CFRunLoop 的时间：让 WindowServer 与
/// `launchservicesd` 提交 `type="UIElement"` 并收走 Dock 图标，再让进程消失。
const DOCK_SETTLE: Duration = Duration::from_millis(50);

/// 谁在请求退出 UI（ADR-0046）。决定前端能否否决这次关闭。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseTrigger {
    /// 红灯 `×`、`⌘Q`、Dock 右键「退出」、`ExitRequested`：前端 `closeWindow()` 可以否决
    /// （未保存设置的确认、上传未完成、发送中），否决后窗口留下，没有超时兜底。
    User,
    /// 菜单栏「退出 <产品名>」→ daemon `agent/stopping`：后台正在停机，`CLOSE_REQUEST_GRACE`
    /// 后无论前端是否回应都退出。
    AgentStopping,
}

/// ADR-0046：所有「请退出 UI」入口的唯一收口——红灯 `×`、`⌘Q`、Dock 右键「退出」、
/// `RunEvent::ExitRequested` 与菜单栏「退出 <产品名>」触发的 `agent/stopping`。
///
/// 写信页装好关闭守卫（`ui_compose_ready`）时先发 `maild://close-request` 让前端刷盘草稿，
/// 前端完成后经 `ui_finish_close` 进 `retire_and_exit`；只有 `CloseTrigger::AgentStopping`
/// 在 `CLOSE_REQUEST_GRACE` 内没有回音时兜底退出。没有守卫时直接退出。任意线程可调，不阻塞调用方。
pub(crate) fn request_close_and_exit(app: &tauri::AppHandle, trigger: CloseTrigger) {
    let state = app.state::<AppState>();
    if state.exiting.load(Ordering::SeqCst) {
        return;
    }
    if !state.guard_ready.load(Ordering::SeqCst) || state.close_allowed.load(Ordering::SeqCst) {
        retire_and_exit(app.clone());
        return;
    }
    if let Err(error) = app.emit("maild://close-request", ()) {
        eprintln!("[maild-ui] 无法通知前端保存草稿，直接退出: {error}");
        retire_and_exit(app.clone());
        return;
    }
    if trigger == CloseTrigger::User {
        return;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CLOSE_REQUEST_GRACE).await;
        if !handle.state::<AppState>().exiting.load(Ordering::SeqCst) {
            eprintln!("[maild-ui] 前端未在宽限期内完成关闭，直接退出");
            retire_and_exit(handle);
        }
    });
}

/// ADR-0046：退出流水线本体，幂等。主线程上同步撤下主窗口（含原生最小化缩略图）并切到
/// `Accessory`，等它完成后再留 `DOCK_SETTLE` 给 runloop 提交，最后 `app.exit(0)`。
///
/// 次序是零残留的关键：进程若以 `type="Foreground"` 死去，Agent 的 `parentASN` 仍指向它，
/// Dock 就会留下「<产品名> 正在后台运行」的幽灵图标。
pub(crate) fn retire_and_exit(app: tauri::AppHandle) {
    {
        let state = app.state::<AppState>();
        state.close_allowed.store(true, Ordering::SeqCst);
        if state.exiting.swap(true, Ordering::SeqCst) {
            return;
        }
    }
    tauri::async_runtime::spawn(async move {
        let (done, retired) = tokio::sync::oneshot::channel();
        let handle = app.clone();
        let scheduled = app.run_on_main_thread(move || {
            let window = handle.get_webview_window("main");
            #[cfg(target_os = "macos")]
            macos_window::demote_to_accessory_on_main_thread(window.as_ref());
            #[cfg(not(target_os = "macos"))]
            if let Some(window) = window {
                let _ = window.hide();
            }
            let _ = done.send(());
        });
        match scheduled {
            Ok(()) => {
                let _ = retired.await;
            }
            Err(error) => eprintln!("[maild-ui] 无法在主线程撤下窗口: {error}"),
        }
        tokio::time::sleep(DOCK_SETTLE).await;
        app.exit(0);
    });
}

/// 主窗口参数（原 `tauri.conf.json` 的 `app.windows[0]`）。改成程序化创建是为了挂导航否决与
/// 弹窗拒绝（ADR-0033 决策 10）；参数不变。
fn main_window_config() -> tauri::utils::config::WindowConfig {
    tauri::utils::config::WindowConfig {
        label: "main".into(),
        title: maild_brand::PRODUCT_NAME.into(),
        width: 1280.0,
        height: 820.0,
        min_width: Some(900.0),
        min_height: Some(640.0),
        center: true,
        title_bar_style: tauri::TitleBarStyle::Overlay,
        hidden_title: true,
        ..Default::default()
    }
}

/// 应用自身的 URL：内嵌形态是 `tauri://localhost`，开发形态是配置里的 `devUrl`（只在 `cfg(dev)` 放行）。
fn app_url_allower(app: &tauri::App) -> impl Fn(&Url) -> bool + Send + Sync + 'static {
    let dev_url = if cfg!(dev) {
        app.config().build.dev_url.clone()
    } else {
        None
    };
    move |url: &Url| {
        if url.scheme() == "tauri" {
            return true;
        }
        match &dev_url {
            Some(dev) => {
                url.scheme() == dev.scheme()
                    && url.host_str() == dev.host_str()
                    && url.port_or_known_default() == dev.port_or_known_default()
            }
            None => false,
        }
    }
}

fn create_main_window(
    app: &tauri::App,
    documents: Arc<DocumentStore>,
    base: ProtocolBase,
) -> tauri::Result<()> {
    let app_allows = app_url_allower(app);
    let handle = app.handle().clone();
    tauri::WebviewWindowBuilder::from_config(app, &main_window_config())?
        // ADR-0033 决策 2/6：wry 对每一个导航动作（含 iframe 子框架）回调。除应用自身与 `/doc/`
        // 之外一律取消；邮件链接翻译成事件，文档原地不动。
        .on_navigation(
            move |url| match navigation_verdict(url, &base, &app_allows) {
                Verdict::Allow => {
                    if url.scheme() == SCHEME {
                        eprintln!("[maild-ui] 放行文档导航 {}", url.scheme());
                    }
                    true
                }
                Verdict::Deny => {
                    eprintln!(
                        "[maild-ui] 拒绝导航: {}://{}",
                        url.scheme(),
                        url.host_str().unwrap_or("")
                    );
                    false
                }
                Verdict::Link { token, index } => {
                    eprintln!("[maild-ui] 邮件链接被点击（第 {index} 项），取消导航并交给确认面板");
                    emit_link(&handle, &documents, &token, index);
                    false
                }
            },
        )
        // sandbox 已经挡了弹窗；这是第二道。
        .on_new_window(|_url, _features| tauri::webview::NewWindowResponse::Deny)
        .build()?;
    Ok(())
}

fn emit_link(app: &tauri::AppHandle, documents: &DocumentStore, token: &str, index: usize) {
    let Some(document) = documents.get(token) else {
        return;
    };
    let Some(link) = document.links.get(index) else {
        return;
    };
    let payload = UiLink {
        token: token.to_string(),
        index,
        href: link.href.clone(),
        kind: link.kind,
        host: link.host.clone(),
    };
    if let Err(error) = app.emit(LINK_EVENT, &payload) {
        eprintln!("[maild-ui] 推送链接事件失败: {error}");
    }
}

/// 连接快照单向推送：每次 `ViewState` 变化投影一次发给 webview；上线那一刻顺带 claim intent、
/// 同步远程图片与翻译代理配置，并全量同步外部翻译插件清单。
fn spawn_state_pump(
    app: tauri::AppHandle,
    bridge: Arc<Bridge>,
    remote_images: Arc<remote_image::RemoteImageFetcher>,
    translation: Arc<TranslationManager>,
) {
    tauri::async_runtime::spawn(async move {
        let mut state = bridge.subscribe();
        let mut was_online = false;
        let mut seen_intent_epoch = 0;
        let mut shutdown_seen = false;
        // 与 actor 初值一致：启动时还没有偏好，webview 与菜单都在基础语言。
        let mut seen_locale: Option<Vec<String>> = None;
        loop {
            let (snapshot, intent_epoch, shutdown_requested, locale) = {
                let current = state.borrow_and_update();
                (
                    bridge.project(&current),
                    current.intent_epoch,
                    current.shutdown_requested,
                    current.locale.clone(),
                )
            };
            let online = snapshot.phase == "online";
            if let Err(error) = app.emit(STATE_EVENT, &snapshot) {
                eprintln!("[maild-ui] 推送连接快照失败: {error}");
            }
            // ADR-0048：daemon 的偏好列表变了才重新解析壳的列表（M2：跟随系统时按本进程的语言，
            // 含系统设置给本 app 单独设的语言），之后转发给 webview、按同一规则重建原生菜单；
            // macOS 上先把模式镜像到本 app 的 AppleLanguages（M1，系统画的文字下次启动跟随）。
            if locale_changed(&mut seen_locale, locale) {
                shell_locale::spawn_refresh(app.clone(), bridge.clone(), seen_locale.clone());
            }
            // ADR-0045：daemon 推来 `frontend/intent`——菜单栏、通知或再次打开主 app bundle 排了意图。
            // 窗口可能正原生最小化在 Dock，或已被别处藏起（`Accessory` 收不到可靠的 Reopen），
            // 这里直接 unminimize 带回前台再领取。
            // ADR-0046：daemon 推来 `agent/stopping`（菜单栏「退出 <产品名>」）——随后台一起离场。
            // 打开着或原生最小化的窗口都经同一条流水线刷盘草稿、撤下 Dock 图标再退出。
            if shutdown_began(&mut shutdown_seen, shutdown_requested) {
                request_close_and_exit(&app, CloseTrigger::AgentStopping);
            }
            if intent_woke(&mut seen_intent_epoch, intent_epoch) {
                show_and_activate_main_window(&app);
                spawn_intent_claim(app.clone(), bridge.clone());
            } else if online && !was_online {
                spawn_intent_claim(app.clone(), bridge.clone());
            }
            if online && !was_online {
                let bridge = bridge.clone();
                let remote_images = remote_images.clone();
                let translation = translation.clone();
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    remote_images.refresh_from_bridge(&bridge, 0).await;
                    translation.refresh_proxy_from_bridge(&bridge, 0).await;
                    if let Ok(providers) = translation.sync_external_providers_from_daemon().await {
                        let _ = app_handle.emit(TRANSLATION_PROVIDERS_EVENT, &providers);
                    }
                });
            }
            if online {
                spawn_compat_check(bridge.clone());
            }
            was_online = online;
            tokio::select! {
                changed = state.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = bridge.compat_changed() => {}
            }
        }
    });
}

/// 转发后台的 `preferences/proxy` 与 `translation/providers` 推送事件给 webview，并热刷新
/// [`remote_image::RemoteImageFetcher`] 与 [`TranslationManager`]（含滞后/重连后的全量同步）。
fn spawn_proxy_pump(
    app: tauri::AppHandle,
    bridge: Arc<Bridge>,
    remote_images: Arc<remote_image::RemoteImageFetcher>,
    translation: Arc<TranslationManager>,
) {
    tauri::async_runtime::spawn(async move {
        let mut events = bridge.subscribe_events();
        loop {
            match events.recv().await {
                Ok(maild_app::AppEvent::Notification(
                    maild_client::ServerNotification::ProxyPreference(event),
                )) => {
                    let generation = event.generation;
                    let payload = settings::UiProxyEvent::from(*event);
                    if let Err(error) = app.emit(PROXY_EVENT, &payload) {
                        eprintln!("[maild-ui] 推送代理偏好事件失败: {error}");
                    }
                    remote_images.refresh_from_bridge(&bridge, generation).await;
                    translation.refresh_proxy_from_bridge(&bridge, generation).await;
                }
                Ok(maild_app::AppEvent::Notification(
                    maild_client::ServerNotification::TranslationProviders(event),
                )) => {
                    if let Some(merged) = translation.apply_external_providers_snapshot(*event) {
                        if let Err(error) = app.emit(TRANSLATION_PROVIDERS_EVENT, &merged) {
                            eprintln!("[maild-ui] 推送翻译插件事件失败: {error}");
                        }
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if let Ok(config) = bridge.proxy_config().await {
                        let payload = settings::UiProxyEvent::from_config(config, 0);
                        let _ = app.emit(PROXY_EVENT, &payload);
                    }
                    remote_images.refresh_from_bridge(&bridge, 0).await;
                    translation.refresh_proxy_from_bridge(&bridge, 0).await;
                    if let Ok(providers) = translation.sync_external_providers_from_daemon().await {
                        let _ = app.emit(TRANSLATION_PROVIDERS_EVENT, &providers);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

/// `intent_epoch` 是否比上次看到的大（ADR-0045）；是则记下新值。
fn intent_woke(seen: &mut u64, current: u64) -> bool {
    if current > *seen {
        *seen = current;
        true
    } else {
        false
    }
}

/// actor 的 `locale` 是否与上次转发的不同（ADR-0048）；是则记下新值。其他快照字段变化不重复转发。
fn locale_changed(seen: &mut Option<Vec<String>>, current: Option<Vec<String>>) -> bool {
    if *seen == current {
        return false;
    }
    *seen = current;
    true
}

/// `shutdown_requested` 是否第一次变成 `true`（ADR-0046）；是则记下。
fn shutdown_began(seen: &mut bool, current: bool) -> bool {
    let began = current && !*seen;
    *seen |= current;
    began
}

/// 每个新连接 epoch 握手一次（ADR-0016）；结论变化由状态泵补推快照，横幅与写入闸门随之更新。
fn spawn_compat_check(bridge: Arc<Bridge>) {
    tauri::async_runtime::spawn(async move {
        bridge.check_compat().await;
    });
}

/// 单轮最多领取的意图数：连续点击菜单或通知时排空队列，又不会因异常的服务端死循环。
const MAX_INTENTS_PER_CLAIM: usize = 16;

/// 排空待领意图并按序推给 webview（ADR-0044）。聚焦、上线与 Reopen 可能同时触发，串行化保证
/// 意图按入队顺序到达——最后一个点击决定最终页面。
fn spawn_intent_claim(app: tauri::AppHandle, bridge: Arc<Bridge>) {
    static CLAIMING: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    tauri::async_runtime::spawn(async move {
        let _serial = CLAIMING
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        if bridge.snapshot().phase != "online" {
            return;
        }
        drain_intents(
            || bridge.claim_intent(),
            |intent| {
                if let Err(error) = app.emit(INTENT_EVENT, &intent) {
                    eprintln!("[maild-ui] 推送导航意图失败: {error}");
                }
            },
        )
        .await;
    });
}

/// 循环 claim 直到队列为空、出错（离线/被拒都不值得打断用户；原因已在桥里进 stderr）或达到
/// 单轮上限。返回本轮推送的条数。
pub async fn drain_intents<T, E, F, Fut>(mut claim: F, mut emit: impl FnMut(T)) -> usize
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>, E>>,
{
    let mut delivered = 0;
    while delivered < MAX_INTENTS_PER_CLAIM {
        match claim().await {
            Ok(Some(intent)) => {
                emit(intent);
                delivered += 1;
            }
            Ok(None) | Err(_) => break,
        }
    }
    delivered
}

#[cfg(test)]
mod intent_tests {
    use super::*;

    #[tokio::test]
    async fn draining_delivers_every_queued_intent_in_order() {
        let queue = std::sync::Mutex::new(std::collections::VecDeque::from([1, 2, 3]));
        let mut seen = Vec::new();
        let delivered = drain_intents(
            || {
                let next = queue.lock().unwrap().pop_front();
                async move { Ok::<_, ()>(next) }
            },
            |intent| seen.push(intent),
        )
        .await;
        assert_eq!((delivered, seen), (3, vec![1, 2, 3]));
    }

    #[test]
    fn each_new_intent_epoch_wakes_the_window_exactly_once() {
        let mut seen = 0;
        assert!(!intent_woke(&mut seen, 0), "初始快照不唤醒");
        assert!(intent_woke(&mut seen, 1));
        assert!(!intent_woke(&mut seen, 1), "同一 epoch 的其他快照变化不重复唤醒");
        assert!(intent_woke(&mut seen, 3), "合并的多次推送只需一次唤醒，claim 会排空队列");
        assert_eq!(seen, 3);
    }

    #[test]
    fn locale_is_forwarded_only_when_the_preference_list_changes() {
        let mut seen = None;
        assert!(!locale_changed(&mut seen, None), "启动时没有偏好，不推送");
        assert!(locale_changed(&mut seen, Some(vec!["en".into()])));
        assert!(
            !locale_changed(&mut seen, Some(vec!["en".into()])),
            "其他快照字段变化不重复推送"
        );
        assert!(locale_changed(&mut seen, Some(vec!["zh-Hans".into(), "en".into()])));
        assert!(locale_changed(&mut seen, None), "daemon 换成老版本：推送 null，webview 回落基础语言");
        assert_eq!(seen, None);
    }

    /// 原生菜单与 webview 用同一条协商规则：繁体中文跳过、空列表回落中文。
    #[test]
    fn native_menu_negotiates_like_the_webview() {
        use maild_l10n::{negotiate, Locale};
        assert_eq!(negotiate::<String>(&[]), Locale::ZhHans);
        assert_eq!(negotiate(&["en-US"]), Locale::En);
        assert_eq!(negotiate(&["zh-Hant-TW", "en"]), Locale::En);
        assert_eq!(negotiate(&["zh-Hans-CN", "en"]), Locale::ZhHans);
        assert_eq!(negotiate(&["fr", "ja"]), Locale::ZhHans);
    }

    #[test]
    fn agent_stopping_triggers_the_exit_pipeline_exactly_once() {
        let mut seen = false;
        assert!(!shutdown_began(&mut seen, false), "未请求停机不退出");
        assert!(shutdown_began(&mut seen, true));
        assert!(!shutdown_began(&mut seen, true), "之后的快照变化不重复触发");
        assert!(seen);
    }

    #[tokio::test]
    async fn draining_is_bounded_and_stops_on_errors() {
        let delivered = drain_intents(|| async { Ok::<_, ()>(Some(0)) }, |_| {}).await;
        assert_eq!(delivered, MAX_INTENTS_PER_CLAIM);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let delivered = drain_intents(
            || {
                calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async { Err::<Option<u8>, _>("offline") }
            },
            |_| panic!("nothing to deliver"),
        )
        .await;
        assert_eq!((delivered, calls.into_inner()), (0, 1));
    }
}
