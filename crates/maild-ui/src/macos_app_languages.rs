//! ADR-0048 决策 8（M1）：把用户的显式语言镜像到本 app 自己偏好域的 `AppleLanguages`。
//!
//! 系统画的文字（WKWebView 右键菜单、打开/保存面板、标准提示按钮、拼写建议）不归我们成文，
//! 由 AppKit 在**进程启动时**按 bundle 声明的 `CFBundleLocalizations` 与本进程的语言列表选定。
//! 系统设置里的「按 app 设置语言」写的正是本 app 偏好域的 `AppleLanguages`，所以壳把 daemon
//! 偏好镜像到这里，系统文字在**下次启动**时跟随：
//!
//! - `explicit`：写入显式列表（daemon 已规范化）；
//! - `system`：删除这个键，回到跟随系统。绝不把系统列表写进来，否则「跟随系统」会被冻结。
//!
//! `preferences/locale` 推送只带 `effective`，不带模式，所以由状态泵在 `effective` 变化时
//! （含启动后首次取到）调 [`spawn_sync`]，经桥重读一次完整偏好再比对写入。只在值确实不同时写。
//! 不监听系统语言变化：跟随系统模式下改了系统语言，重启 app 生效（ADR 定案）。
//!
//! 只读写本 app 的域（`kCFPreferencesCurrentApplication`，打包后是 bundle id，`cargo run` 是可执行
//! 文件名），从不碰全局域。测试经 [`AppLanguageStore`] 注入假存储，不触碰真实用户默认值。

use std::sync::{Arc, OnceLock};

use objc2_core_foundation::{
    kCFPreferencesAnyHost, kCFPreferencesCurrentApplication, kCFPreferencesCurrentUser, CFArray,
    CFPreferencesCopyValue, CFPreferencesSetValue, CFPreferencesSynchronize, CFRetained, CFString,
    CFType,
};

use crate::bridge::{Bridge, UiLocalePreference};

const APPLE_LANGUAGES: &str = "AppleLanguages";

/// 本 app 偏好域里的 `AppleLanguages`。真实实现见 [`UserDefaults`]；测试用内存实现。
pub(crate) trait AppLanguageStore {
    /// 本域自己的值；只看本域，不回落到全局域。不是字符串数组时视为缺失。
    fn get(&self) -> Option<Vec<String>>;
    fn set(&self, languages: &[String]);
    fn remove(&self);
}

/// 一次镜像做了什么（测试与日志用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mirror {
    Unchanged,
    Written,
    Removed,
    /// 偏好形状不认识（未知模式、显式却没有语言）：不动存储。
    Skipped,
}

/// 按偏好模式决定 `AppleLanguages` 应有的值，与存储里的不同才写。
pub(crate) fn mirror(store: &impl AppLanguageStore, preference: &UiLocalePreference) -> Mirror {
    let wanted = match preference.mode.as_str() {
        "explicit" if !preference.languages.is_empty() => Some(preference.languages.as_slice()),
        "system" => None,
        _ => return Mirror::Skipped,
    };
    let current = store.get();
    if current.as_deref() == wanted {
        return Mirror::Unchanged;
    }
    match wanted {
        Some(languages) => {
            store.set(languages);
            Mirror::Written
        }
        None => {
            store.remove();
            Mirror::Removed
        }
    }
}

/// 经桥重读完整偏好并镜像。串行执行：连续变化时后一次在前一次写完后重读，最后一次总是最新值。
pub(crate) fn spawn_sync(bridge: Arc<Bridge>) {
    static SYNCING: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    tauri::async_runtime::spawn(async move {
        let _serial = SYNCING
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        // 失败（老 daemon、断线）不动存储；桥已记日志，下次 `effective` 变化或下次启动再同步。
        let Ok(preference) = bridge.locale_preference().await else {
            return;
        };
        match mirror(&UserDefaults, &preference) {
            Mirror::Written => {
                eprintln!(
                    "[maild-ui] 系统界面语言下次启动生效: {:?}",
                    preference.languages
                )
            }
            Mirror::Removed => eprintln!("[maild-ui] 系统界面语言下次启动起跟随系统"),
            Mirror::Unchanged | Mirror::Skipped => {}
        }
    });
}

/// 本进程的应用偏好域（CFPreferences，与 `NSUserDefaults` 的应用域同一份）。
struct UserDefaults;

impl UserDefaults {
    fn key() -> CFRetained<CFString> {
        CFString::from_str(APPLE_LANGUAGES)
    }

    /// (本 app, 当前用户, 任意主机)：`NSUserDefaults` 应用域与系统设置按 app 语言写的位置。
    fn domain() -> (&'static CFString, &'static CFString, &'static CFString) {
        // SAFETY: CoreFoundation 导出的常量字符串，进程内只读且永久有效。
        unsafe {
            (
                kCFPreferencesCurrentApplication,
                kCFPreferencesCurrentUser,
                kCFPreferencesAnyHost,
            )
        }
    }

    fn write(value: Option<&CFType>) {
        let (application, user, host) = Self::domain();
        // SAFETY: 值是 CFString 组成的 CFArray 或 NULL（删除键），都是合法的属性列表。
        unsafe { CFPreferencesSetValue(&Self::key(), value, application, user, host) };
        if !CFPreferencesSynchronize(application, user, host) {
            eprintln!("[maild-ui] 写入本 app 的 AppleLanguages 失败");
        }
    }
}

impl AppLanguageStore for UserDefaults {
    fn get(&self) -> Option<Vec<String>> {
        let (application, user, host) = Self::domain();
        // CopyValue 只查给定的域；CopyAppValue 会回落到全局域，不能用。
        let value = CFPreferencesCopyValue(&Self::key(), application, user, host)?;
        let array = value.downcast_ref::<CFArray>()?;
        // SAFETY: CFArray 的元素都是 CF 对象；下面逐个检查是不是字符串。
        let array = unsafe { array.cast_unchecked::<CFType>() };
        array
            .iter()
            .map(|item| item.downcast_ref::<CFString>().map(ToString::to_string))
            .collect()
    }

    fn set(&self, languages: &[String]) {
        let strings: Vec<_> = languages
            .iter()
            .map(|tag| CFString::from_str(tag))
            .collect();
        let array = CFArray::from_retained_objects(&strings);
        Self::write(Some(&array));
    }

    fn remove(&self) {
        Self::write(None);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    struct Memory {
        value: RefCell<Option<Vec<String>>>,
        writes: RefCell<usize>,
    }

    impl AppLanguageStore for Memory {
        fn get(&self) -> Option<Vec<String>> {
            self.value.borrow().clone()
        }
        fn set(&self, languages: &[String]) {
            *self.writes.borrow_mut() += 1;
            *self.value.borrow_mut() = Some(languages.to_vec());
        }
        fn remove(&self) {
            *self.writes.borrow_mut() += 1;
            *self.value.borrow_mut() = None;
        }
    }

    fn preference(mode: &str, languages: &[&str], effective: &[&str]) -> UiLocalePreference {
        UiLocalePreference {
            mode: mode.into(),
            languages: languages.iter().map(|tag| (*tag).into()).collect(),
            effective: effective.iter().map(|tag| (*tag).into()).collect(),
            persisted: true,
        }
    }

    #[test]
    fn explicit_mode_writes_the_explicit_list_once() {
        let store = Memory::default();
        let english = preference("explicit", &["en"], &["en"]);
        assert_eq!(mirror(&store, &english), Mirror::Written);
        assert_eq!(store.get(), Some(vec!["en".to_string()]));
        assert_eq!(
            mirror(&store, &english),
            Mirror::Unchanged,
            "值没变不重复写"
        );
        assert_eq!(*store.writes.borrow(), 1);

        let ordered = preference("explicit", &["zh-Hans", "en"], &["zh-Hans", "en"]);
        assert_eq!(mirror(&store, &ordered), Mirror::Written);
        assert_eq!(
            store.get(),
            Some(vec!["zh-Hans".to_string(), "en".to_string()])
        );
    }

    #[test]
    fn system_mode_removes_the_key_and_never_writes_the_system_list() {
        let store = Memory::default();
        *store.value.borrow_mut() = Some(vec!["en".into()]);
        let system = preference("system", &[], &["zh-Hans-CN", "en-US"]);
        assert_eq!(mirror(&store, &system), Mirror::Removed);
        assert_eq!(store.get(), None, "跟随系统：删键，不写 effective");
        assert_eq!(
            mirror(&store, &system),
            Mirror::Unchanged,
            "已经没有键，不再写"
        );
        assert_eq!(*store.writes.borrow(), 1);
    }

    #[test]
    fn a_per_app_language_set_elsewhere_is_replaced_by_the_daemon_preference() {
        // 系统设置里给本 app 单独设的语言：显式模式被覆盖，跟随系统模式被删除（已知、已接受的副作用）。
        let store = Memory::default();
        *store.value.borrow_mut() = Some(vec!["ja".into()]);
        assert_eq!(
            mirror(&store, &preference("explicit", &["en"], &["en"])),
            Mirror::Written
        );
        *store.value.borrow_mut() = Some(vec!["ja".into()]);
        assert_eq!(
            mirror(&store, &preference("system", &[], &["en"])),
            Mirror::Removed
        );
    }

    #[test]
    fn unknown_preference_shapes_leave_the_store_alone() {
        let store = Memory::default();
        *store.value.borrow_mut() = Some(vec!["en".into()]);
        assert_eq!(
            mirror(&store, &preference("explicit", &[], &["en"])),
            Mirror::Skipped
        );
        assert_eq!(
            mirror(&store, &preference("future", &["en"], &["en"])),
            Mirror::Skipped
        );
        assert_eq!(store.get(), Some(vec!["en".to_string()]));
        assert_eq!(*store.writes.borrow(), 0);
    }
}
