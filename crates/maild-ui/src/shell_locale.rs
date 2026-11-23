//! ADR-0048 决策 8（M2）：壳自己的语言列表——webview 文案、原生菜单与设置页读的都是它。
//!
//! M1 之后两个前端都出现在系统设置「语言与地区 → 应用」里。用户若在那里给本 app 单独设了语言，
//! 而我们的偏好是「跟随系统」，系统画的文字（右键菜单、文件面板）已经按它走；我们自己画的界面也该
//! 一致。daemon 的 `effective` 是 Agent 进程读到的全局系统列表，看不到别的 app 的按 app 设置，所以：
//!
//! - `system`：用**本进程**的首选语言（`CFLocaleCopyPreferredLanguages`，已经包含本 app 偏好域的
//!   `AppleLanguages`，同一进程里改了键立即可见）；为空时回落 daemon 的 `effective`。
//! - `explicit`：照旧用 daemon 的 `effective`。
//! - 模式未知（老 daemon、还没取到偏好）：照旧用 daemon 的 `effective`，即今天的行为。
//!
//! 只影响本前端 app：daemon、通知、菜单栏与启动器仍跟随全局列表，这正是 macOS「按 app 设置」的语义。
//! 不监听系统语言或按 app 语言的变化（不轮询、不订阅通知），改了重启 app 生效。
//!
//! 与 M1 镜像的顺序：`system` 模式下镜像可能先删掉我们以前写的过期值（偏好在本 app 未运行时被别的
//! 客户端改回跟随系统），所以一律**先镜像、后读本进程语言**，过期的镜像值不会被当成用户的按 app
//! 选择。镜像、读取与转发共用一把串行锁。

use std::sync::{Arc, Mutex, OnceLock};

use tauri::{AppHandle, Emitter as _, Manager as _};

use crate::bridge::{Bridge, UiError, UiLocale, UiLocalePreference, LOCALE_EVENT};
use crate::AppState;

/// 本进程首选语言最多取几项，与 daemon `effective` 的上限相同，使事件载荷有界。
const MAX_LANGUAGES: usize = 8;

/// 只有 macOS 读得到本进程的语言（也只有它要镜像）；其他平台壳的列表恒等于 daemon 的，
/// 不必为取模式多一次往返。
const READS_OWN_LANGUAGES: bool = cfg!(target_os = "macos");

/// 壳的语言列表（纯函数）。`process` 只在 `system` 模式下调用；结果为 `None` 表示 daemon 也没有
/// 给出列表（老 daemon、尚未取到），webview 回落基础语言。
pub(crate) fn resolve(
    mode: Option<&str>,
    daemon: Option<&[String]>,
    process: impl FnOnce() -> Vec<String>,
) -> Option<Vec<String>> {
    if mode == Some("system") {
        let own: Vec<String> = process()
            .into_iter()
            .filter(|tag| !tag.is_empty())
            .take(MAX_LANGUAGES)
            .collect();
        if !own.is_empty() {
            return Some(own);
        }
    }
    daemon.map(<[String]>::to_vec)
}

/// 拿到完整偏好之后：先 `mirror`（M1 镜像，可能删掉过期值），再按本进程语言解析，应答的
/// `effective` 换成壳的列表。失败原样返回，两个回调都不调用。
pub(crate) fn settle(
    result: Result<UiLocalePreference, UiError>,
    mirror: impl FnOnce(&UiLocalePreference),
    process: impl FnOnce() -> Vec<String>,
) -> Result<UiLocalePreference, UiError> {
    let mut preference = result?;
    mirror(&preference);
    if let Some(effective) = resolve(Some(&preference.mode), Some(&preference.effective), process) {
        preference.effective = effective;
    }
    Ok(preference)
}

/// 壳手里的语言列表与最近一次得知的偏好模式。存在 `AppState`，`ui_locale` 直接读。
#[derive(Default)]
pub struct ShellLocale {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// 最近一次转发给 webview 的列表；启动时 `None`，与 webview、原生菜单的基础语言一致。
    effective: Option<Vec<String>>,
    /// 最近一次得知的模式。取偏好偶尔失败（超时、断线）时沿用它，不在两种列表之间来回跳。
    mode: Option<String>,
}

/// 壳的列表变了：转发给 webview 的载荷，以及协商结果变了时原生菜单要换成的语言。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub locale: UiLocale,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub menu: Option<maild_l10n::Locale>,
}

impl ShellLocale {
    /// `ui_locale` 的结果：webview 启动时取一次，之后跟 [`LOCALE_EVENT`]。不发请求。
    pub fn current(&self) -> UiLocale {
        UiLocale {
            effective: self.lock().effective.clone(),
        }
    }

    fn mode(&self) -> Option<String> {
        self.lock().mode.clone()
    }

    fn set_mode(&self, mode: Option<String>) {
        self.lock().mode = mode;
    }

    /// 记下新列表；与上次相同返回 `None`（不重复转发）。
    pub(crate) fn publish(&self, effective: Option<Vec<String>>) -> Option<Change> {
        let mut inner = self.lock();
        if inner.effective == effective {
            return None;
        }
        let before = maild_l10n::negotiate(inner.effective.as_deref().unwrap_or_default());
        let after = maild_l10n::negotiate(effective.as_deref().unwrap_or_default());
        inner.effective = effective;
        Some(Change {
            locale: UiLocale {
                effective: inner.effective.clone(),
            },
            menu: (before != after).then_some(after),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 镜像、读本进程语言与转发的串行锁：后到的一次在前一次转发完之后执行，最后一次总是最新值。
fn serial() -> &'static tokio::sync::Mutex<()> {
    static SETTLING: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    SETTLING.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// 本进程的首选语言（含本 app 偏好域的 `AppleLanguages`）。其他平台没有按 app 语言，返回空列表，
/// 于是壳的列表就是 daemon 的。
fn process_languages() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        crate::macos_app_languages::process_languages()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// M1：把完整偏好镜像到本 app 的 `AppleLanguages`（仅 macOS）。
fn mirror(preference: &UiLocalePreference) {
    #[cfg(target_os = "macos")]
    crate::macos_app_languages::sync(preference);
    #[cfg(not(target_os = "macos"))]
    let _ = preference;
}

/// 列表变了才转发给 webview；协商结果变了才重建原生菜单。
fn publish(app: &AppHandle, shell: &ShellLocale, effective: Option<Vec<String>>) {
    let Some(change) = shell.publish(effective) else {
        return;
    };
    if let Err(error) = app.emit(LOCALE_EVENT, &change.locale) {
        eprintln!("[maild-ui] 推送语言偏好失败: {error}");
    }
    #[cfg(target_os = "macos")]
    if let Some(locale) = change.menu {
        crate::macos_locale::apply(app, locale);
    }
}

/// 状态泵看到 daemon 的 `effective` 变了（含启动后首次取到）：推送只带 `effective`，所以经桥再取
/// 一次完整偏好拿到模式 → 镜像 → 读本进程语言 → 转发。
pub(crate) fn spawn_refresh(app: AppHandle, bridge: Arc<Bridge>, daemon: Option<Vec<String>>) {
    tauri::async_runtime::spawn(async move {
        let _serial = serial().lock().await;
        let shell = &app.state::<AppState>().locale;
        let effective = match daemon {
            // 老 daemon（或换成了老版本）：没有偏好，也就没有模式；不碰镜像。
            None => {
                shell.set_mode(None);
                None
            }
            Some(daemon) if !READS_OWN_LANGUAGES => Some(daemon),
            Some(daemon) => match bridge.locale_preference().await {
                Ok(preference) => {
                    shell.set_mode(Some(preference.mode.clone()));
                    settle(Ok(preference), mirror, process_languages)
                        .ok()
                        .map(|preference| preference.effective)
                }
                // 取偏好失败不动镜像（桥已记日志）。老 daemon 没有模式；其他失败沿用上次的模式。
                Err(error) => {
                    if error.reason == "locale_unsupported" {
                        shell.set_mode(None);
                    }
                    resolve(shell.mode().as_deref(), Some(&daemon), process_languages)
                }
            },
        };
        publish(&app, shell, effective);
    });
}

/// `ui_set_locale_preference` 成功后：按应答镜像（`effective` 不变时 daemon 不推送，只能靠这一次）、
/// 再按本进程语言解析并转发；应答的 `effective` 换成壳的列表——webview 收到应答立即按它切换。
pub(crate) async fn after_set(
    app: &AppHandle,
    result: Result<UiLocalePreference, UiError>,
) -> Result<UiLocalePreference, UiError> {
    let _serial = serial().lock().await;
    let result = settle(result, mirror, process_languages);
    if let Ok(preference) = &result {
        let shell = &app.state::<AppState>().locale;
        shell.set_mode(Some(preference.mode.clone()));
        publish(app, shell, Some(preference.effective.clone()));
    }
    result
}

/// `ui_locale_preference`：设置页看到的 `effective` 同样是壳的列表。只读，不镜像、不转发。
pub(crate) async fn for_settings(
    result: Result<UiLocalePreference, UiError>,
) -> Result<UiLocalePreference, UiError> {
    let _serial = serial().lock().await;
    settle(result, |_| {}, process_languages)
}

#[cfg(test)]
mod tests {
    use maild_l10n::Locale;

    use super::*;

    fn strings(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|tag| (*tag).into()).collect()
    }

    fn preference(mode: &str, languages: &[&str], effective: &[&str]) -> UiLocalePreference {
        UiLocalePreference {
            mode: mode.into(),
            languages: strings(languages),
            effective: strings(effective),
            persisted: true,
        }
    }

    #[test]
    fn system_mode_prefers_this_process_languages() {
        // 系统是中文，用户在系统设置里给本 app 单独设了英文。
        let daemon = strings(&["zh-Hans-CN"]);
        assert_eq!(
            resolve(Some("system"), Some(&daemon), || strings(&["en"])),
            Some(strings(&["en"]))
        );
        // 没有按 app 设置：本进程读到的就是全局列表，与 daemon 一致。
        assert_eq!(
            resolve(Some("system"), Some(&daemon), || strings(&[
                "zh-Hans-CN",
                "en-US"
            ])),
            Some(strings(&["zh-Hans-CN", "en-US"]))
        );
    }

    #[test]
    fn system_mode_falls_back_to_the_daemon_when_this_process_has_no_languages() {
        let daemon = strings(&["zh-Hans-CN", "en-US"]);
        assert_eq!(
            resolve(Some("system"), Some(&daemon), Vec::new),
            Some(daemon.clone())
        );
        assert_eq!(
            resolve(Some("system"), Some(&daemon), || strings(&[""])),
            Some(daemon),
            "空标签不算"
        );
    }

    #[test]
    fn this_process_languages_are_bounded_like_the_daemon_list() {
        let many: Vec<String> = (0..12).map(|index| format!("x{index}")).collect();
        let resolved = resolve(Some("system"), Some(&strings(&["en"])), || many.clone()).unwrap();
        assert_eq!(resolved, many[..MAX_LANGUAGES]);
    }

    #[test]
    fn explicit_mode_ignores_this_process_languages() {
        let daemon = strings(&["en"]);
        assert_eq!(
            resolve(Some("explicit"), Some(&daemon), || panic!(
                "显式模式不读本进程语言"
            )),
            Some(daemon)
        );
    }

    #[test]
    fn unknown_mode_keeps_todays_behaviour() {
        let daemon = strings(&["en-US"]);
        let never = || -> Vec<String> { panic!("模式未知不读本进程语言") };
        assert_eq!(resolve(None, Some(&daemon), never), Some(daemon.clone()));
        assert_eq!(resolve(Some("future"), Some(&daemon), never), Some(daemon));
        assert_eq!(
            resolve(None, None, never),
            None,
            "老 daemon：webview 回落基础语言"
        );
    }

    #[test]
    fn settle_mirrors_before_reading_and_rewrites_the_reply() {
        let order = std::cell::RefCell::new(Vec::new());
        let reply = settle(
            Ok(preference("system", &[], &["zh-Hans-CN"])),
            |_| order.borrow_mut().push("mirror"),
            || {
                order.borrow_mut().push("process");
                strings(&["en"])
            },
        )
        .unwrap();
        assert_eq!(order.into_inner(), ["mirror", "process"]);
        assert_eq!(
            reply.effective,
            strings(&["en"]),
            "设置页与写入应答也带壳的列表"
        );
        assert_eq!(reply.mode, "system");
        assert!(reply.languages.is_empty());

        let explicit = settle(
            Ok(preference("explicit", &["zh-Hans"], &["zh-Hans"])),
            |_| {},
            || panic!("显式模式不读本进程语言"),
        )
        .unwrap();
        assert_eq!(explicit.effective, strings(&["zh-Hans"]));
    }

    #[test]
    fn a_failed_read_or_write_is_returned_untouched() {
        let failed = settle(
            Err(UiError::invalid("locale_unsupported", "x")),
            |_| panic!("失败不镜像"),
            || panic!("失败不读本进程语言"),
        );
        assert_eq!(failed.unwrap_err().reason, "locale_unsupported");
    }

    #[test]
    fn ui_locale_and_the_event_carry_the_shell_list() {
        let shell = ShellLocale::default();
        assert_eq!(shell.current(), UiLocale { effective: None });

        // daemon 说中文，本 app 按 app 设成英文：转发与 `ui_locale` 都是英文，菜单换成英文。
        let daemon = strings(&["zh-Hans-CN"]);
        let own = resolve(Some("system"), Some(&daemon), || strings(&["en"]));
        let change = shell.publish(own).expect("列表变了要转发");
        assert_eq!(change.locale.effective, Some(strings(&["en"])));
        assert_eq!(change.menu, Some(Locale::En));
        assert_eq!(shell.current(), change.locale);

        assert_eq!(
            shell.publish(Some(strings(&["en"]))),
            None,
            "同一列表不重复转发"
        );
        let change = shell
            .publish(Some(strings(&["en-GB", "zh-Hans"])))
            .expect("列表变了要转发");
        assert_eq!(change.menu, None, "协商结果没变不重建菜单");

        let change = shell.publish(None).expect("换成老 daemon：推送 null");
        assert_eq!(change.locale.effective, None);
        assert_eq!(change.menu, Some(Locale::ZhHans), "回落基础语言");
    }
}
