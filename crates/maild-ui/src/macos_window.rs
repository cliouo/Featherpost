//! macOS 前台激活与退出收敛（ADR-0045，ADR-0046）。
//!
//! 黄灯 `–` 与 `⌘M` 走 100% 系统原生最小化（Dock 保留图标与废纸篓旁的窗口缩略图），这里不碰标题栏
//! 按钮与应用菜单。只提供几件小事：窗口回到前台时激活应用，撤下窗口
//! 后把前台还给上一个应用，以及把 `[NSApp terminate:]`（Dock 右键「退出」、`⌘Q`）拦进与红灯 `×`
//! 同一条「刷盘草稿 → 降级 `Accessory` → `app.exit(0)`」流水线。
use std::ffi::c_char;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;

use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{class, msg_send, sel, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

/// 让本进程成为前台应用（`Accessory → Regular` 之后需要显式激活，窗口才会拿到键盘焦点）。
pub(crate) fn activate_app() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    // `activate` 只在 macOS 14+ 存在；旧 API 在所有受支持版本上都可用。
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

/// ADR-0046：同步撤下主窗口（`orderOut:` 同时收走普通窗口与原生最小化在 Dock 的缩略图）并把进程
/// 降为 `Accessory`（`launchservicesd` 视角 `type="UIElement"`），再交还前台。必须在主线程调用；
/// 不在主线程时什么也不做。幂等：退出流水线与 `RunEvent::Exit` 兜底都会调。
pub(crate) fn demote_to_accessory_on_main_thread(window: Option<&tauri::WebviewWindow>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if let Some(window) = window {
        let _ = window.hide();
    }
    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    ns_app.deactivate();
}

/// `applicationShouldTerminate:` 回调要找回的应用句柄。
static TERMINATE_APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// `NSApplicationTerminateReply`。
const NS_TERMINATE_CANCEL: usize = 0;
const NS_TERMINATE_NOW: usize = 1;

/// AppleEvent 属性 `keyAEQuitReason`（`'why?'`）：只有注销、重启、关机发出的退出事件才带它。
const KEY_AE_QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");

/// ADR-0046：给 tao 的 app delegate（`TaoAppDelegateParent`，它没有实现
/// `applicationShouldTerminate:`）运行时补上这个方法。必须在主线程（`.setup()`）调用；
/// 重复调用无副作用。
///
/// 没有它时，Dock 右键「退出」/ `⌘Q` 走 `[NSApp terminate:]` → AppKit 默认 `NSTerminateNow`
/// → 直接 `applicationWillTerminate:` 结束进程，既绕过 `RunEvent::ExitRequested`（草稿不刷盘），
/// 也来不及切回 `Accessory`——进程以 `type="Foreground"` 死去，而 Agent 的 `parentASN` 指向它，
/// `launchservicesd` 于是把它记成 `exited-with-subordinates`，Dock 留下「正在后台运行」的幽灵图标。
pub(crate) fn install_terminate_handler(app: tauri::AppHandle) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("[maild-ui] 退出拦截必须在主线程安装");
        return;
    };
    if TERMINATE_APP.set(app).is_err() {
        return;
    }
    let ns_app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = ns_app.delegate() else {
        eprintln!("[maild-ui] NSApp 没有 delegate，Dock「退出」/ ⌘Q 不经过退出流水线");
        return;
    };
    let object: &AnyObject = delegate.as_ref();
    let cls = object.class() as *const AnyClass as *mut AnyClass;
    // SAFETY: `application_should_terminate` 的签名与类型编码 `Q@:@` 一致
    // （`NSApplicationTerminateReply` 是 `NSUInteger`）；`Imp` 只是擦除了签名的函数指针。
    let added = unsafe {
        let imp: Imp = std::mem::transmute::<
            extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize,
            Imp,
        >(application_should_terminate);
        objc2::ffi::class_addMethod(
            cls,
            sel!(applicationShouldTerminate:),
            imp,
            c"Q@:@".as_ptr() as *const c_char,
        )
    };
    if !added.as_bool() {
        eprintln!("[maild-ui] delegate 已实现 applicationShouldTerminate:，保留原实现");
        return;
    }
    // NSApplication 在 `setDelegate:` 时缓存 delegate 的 `respondsToSelector:` 结果；运行时补上的
    // 方法要重设一次才会被看见。tao 自己持有 delegate 的强引用，这里的 `delegate` 也保证交换期间存活。
    ns_app.setDelegate(None);
    ns_app.setDelegate(Some(&delegate));
}

/// `-[NSApplicationDelegate applicationShouldTerminate:]`。
///
/// 常规退出（Dock「退出」、`⌘Q`、AppleScript `quit`）一律返回 `NSTerminateCancel`，改走
/// `request_close_and_exit`（用户发起，前端可否决）：tao 的 `app.exit(0)` 用的是 `[NSApp stop:]`
/// 而不是 `terminate:`，取消这次 terminate 既不弹「强制退出」也不妨碍随后的退出。
///
/// 注销 / 重启 / 关机例外：在那种场合返回 Cancel 会让系统提示「<产品名> 中断了注销」。此时同步降级
/// `Accessory` 后放行 `NSTerminateNow`（与 ADR-0046 之前一样来不及刷盘草稿，但不再留幽灵图标）。
extern "C-unwind" fn application_should_terminate(
    _this: &AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
) -> usize {
    let Some(app) = TERMINATE_APP.get() else {
        return NS_TERMINATE_NOW;
    };
    if system_session_ending() {
        use tauri::Manager as _;
        let state = app.state::<crate::AppState>();
        state.close_allowed.store(true, Ordering::SeqCst);
        state.exiting.store(true, Ordering::SeqCst);
        demote_to_accessory_on_main_thread(app.get_webview_window("main").as_ref());
        return NS_TERMINATE_NOW;
    }
    crate::request_close_and_exit(app, crate::CloseTrigger::User);
    NS_TERMINATE_CANCEL
}

/// 当前正在处理的 AppleEvent 是否带 `keyAEQuitReason`（系统注销、重启、关机）。菜单 `⌘Q`
/// 没有当前 AppleEvent；Dock「退出」的 `kAEQuitApplication` 不带退出原因。
fn system_session_ending() -> bool {
    // SAFETY: 只在主线程（AppKit 调用 delegate 时）进入；两个消息都是公开 Foundation API，
    // 返回值按可空对象指针处理。
    unsafe {
        let manager: *mut AnyObject =
            msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
        let Some(manager) = manager.as_ref() else {
            return false;
        };
        let event: *mut AnyObject = msg_send![manager, currentAppleEvent];
        let Some(event) = event.as_ref() else {
            return false;
        };
        let reason: *mut AnyObject =
            msg_send![event, attributeDescriptorForKeyword: KEY_AE_QUIT_REASON];
        !reason.is_null()
    }
}
