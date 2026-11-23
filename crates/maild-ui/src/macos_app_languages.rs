//! ADR-0048 决策 8（M1）：把用户的显式语言镜像到本 app 自己偏好域的 `AppleLanguages`。
//!
//! 系统画的文字（WKWebView 右键菜单、打开/保存面板、标准提示按钮、拼写建议）不归我们成文，
//! 由 AppKit 在**进程启动时**按 bundle 声明的 `CFBundleLocalizations` 与本进程的语言列表选定。
//! 系统设置里的「按 app 设置语言」写的正是本 app 偏好域的 `AppleLanguages`，所以壳把 daemon
//! 偏好镜像到这里，系统文字在**下次启动**时跟随。镜像只管自己写过的值：
//!
//! - `explicit`：`AppleLanguages` 写显式列表（daemon 已规范化），同时把同一值写进伴随键
//!   [`MIRRORED_KEY`]，记下「这是我们写的」；
//! - `system`：只有伴随键存在且 `AppleLanguages` 仍等于它时才两个一起删（我们写的）；不等或没有
//!   伴随键，说明值来自系统设置或用户自己，`AppleLanguages` 原样保留，只清掉过期的伴随键。
//!   绝不把系统列表写进来，否则「跟随系统」会被冻结。
//!
//! 触发（由 `shell_locale` 调 [`sync`]，与 M2 的解析共用一把串行锁）：`preferences/locale` 推送只带
//! `effective`，不带模式，所以状态泵在 `effective` 变化时（含启动后首次取到）经桥重读一次完整偏好；
//! 本壳自己的 `ui_set_locale_preference` 成功后再按应答直接同步一次，覆盖显式列表恰好等于系统列表、
//! `effective` 不变也就没有推送的情况。只在值确实不同时写。镜像之后才读 [`process_languages`]（M2），
//! 过期的镜像值不会被当成用户的按 app 选择。
//! 不监听系统语言变化：跟随系统模式下改了系统语言，重启 app 生效（ADR 定案）。
//!
//! 只读写本 app 的域（`kCFPreferencesCurrentApplication`，打包后是 bundle id，`cargo run` 是可执行
//! 文件名），从不碰全局域。测试经 [`AppLanguageStore`] 注入假存储，不触碰真实用户默认值。

use objc2_core_foundation::{
    kCFPreferencesAnyHost, kCFPreferencesCurrentApplication, kCFPreferencesCurrentUser, CFArray,
    CFLocale, CFPreferencesCopyValue, CFPreferencesSetValue, CFPreferencesSynchronize, CFString,
    CFType,
};

use crate::bridge::UiLocalePreference;

/// 系统设置「按 app 设置语言」与 AppKit 读的键。
pub(crate) const APPLE_LANGUAGES: &str = "AppleLanguages";
/// 伴随键：最近一次由我们写进 `AppleLanguages` 的值。用代号而不是品牌名，与 ADR-0047 一致。
pub(crate) const MIRRORED_KEY: &str = "MaildMirroredAppleLanguages";

/// 本 app 偏好域里的字符串数组键。真实实现见 [`UserDefaults`]；测试用内存实现。
pub(crate) trait AppLanguageStore {
    /// 本域自己的值；只看本域，不回落到全局域。不是字符串数组时视为缺失。
    fn get(&self, key: &str) -> Option<Vec<String>>;
    fn set(&self, key: &str, languages: &[String]);
    fn remove(&self, key: &str);
}

/// 一次镜像做了什么（测试与日志用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mirror {
    Unchanged,
    /// 显式模式：写入（或补齐）`AppleLanguages` 与伴随键。
    Written,
    /// 跟随系统：删掉我们自己写的 `AppleLanguages` 与伴随键。
    Removed,
    /// 跟随系统：`AppleLanguages` 不是我们写的，保留；只清掉过期的伴随键。
    CompanionCleared,
    /// 偏好形状不认识（未知模式、显式却没有语言）：不动存储。
    Skipped,
}

/// 按偏好模式镜像 `AppleLanguages`，只动自己写过的值。
pub(crate) fn mirror(store: &impl AppLanguageStore, preference: &UiLocalePreference) -> Mirror {
    let mirrored = store.get(MIRRORED_KEY);
    match preference.mode.as_str() {
        "explicit" if !preference.languages.is_empty() => {
            let wanted = preference.languages.as_slice();
            let mut wrote = false;
            if store.get(APPLE_LANGUAGES).as_deref() != Some(wanted) {
                store.set(APPLE_LANGUAGES, wanted);
                wrote = true;
            }
            if mirrored.as_deref() != Some(wanted) {
                store.set(MIRRORED_KEY, wanted);
                wrote = true;
            }
            if wrote {
                Mirror::Written
            } else {
                Mirror::Unchanged
            }
        }
        "system" => {
            let Some(mirrored) = mirrored else {
                return Mirror::Unchanged;
            };
            if store.get(APPLE_LANGUAGES).as_ref() == Some(&mirrored) {
                store.remove(APPLE_LANGUAGES);
                store.remove(MIRRORED_KEY);
                Mirror::Removed
            } else {
                store.remove(MIRRORED_KEY);
                Mirror::CompanionCleared
            }
        }
        _ => Mirror::Skipped,
    }
}

fn log(mirror: Mirror, preference: &UiLocalePreference) {
    match mirror {
        Mirror::Written => eprintln!(
            "[maild-ui] 系统界面语言下次启动生效: {:?}",
            preference.languages
        ),
        Mirror::Removed => eprintln!("[maild-ui] 系统界面语言下次启动起跟随系统"),
        Mirror::CompanionCleared => {
            eprintln!("[maild-ui] AppleLanguages 不是本壳写的，跟随系统时保留")
        }
        Mirror::Unchanged | Mirror::Skipped => {}
    }
}

/// 按完整偏好镜像本进程的应用偏好域。调用方（`shell_locale`）持串行锁，并在这之后才读
/// [`process_languages`]（M2）。
pub(crate) fn sync(preference: &UiLocalePreference) {
    log(mirror(&UserDefaults, preference), preference);
}

/// 本进程的首选语言（M2）：`CFLocaleCopyPreferredLanguages`，与 `+[NSLocale preferredLanguages]`
/// 同源。本 app 偏好域有 `AppleLanguages`（系统设置的按 app 语言，或 M1 写的显式列表）时就是它，
/// 否则是全局列表；同一进程里改了键立即可见（M2 用一次性探针核验过）。
pub(crate) fn process_languages() -> Vec<String> {
    let Some(array) = CFLocale::preferred_languages() else {
        return Vec::new();
    };
    // SAFETY: CFArray 的元素都是 CF 对象；下面逐个检查是不是字符串。
    let array = unsafe { array.cast_unchecked::<CFType>() };
    array
        .iter()
        .filter_map(|item| item.downcast_ref::<CFString>().map(ToString::to_string))
        .collect()
}

/// 本进程的应用偏好域（CFPreferences，与 `NSUserDefaults` 的应用域同一份）。
struct UserDefaults;

impl UserDefaults {
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

    fn write(key: &str, value: Option<&CFType>) {
        let (application, user, host) = Self::domain();
        // SAFETY: 值是 CFString 组成的 CFArray 或 NULL（删除键），都是合法的属性列表。
        unsafe { CFPreferencesSetValue(&CFString::from_str(key), value, application, user, host) };
        if !CFPreferencesSynchronize(application, user, host) {
            eprintln!("[maild-ui] 写入本 app 的 {key} 失败");
        }
    }
}

impl AppLanguageStore for UserDefaults {
    fn get(&self, key: &str) -> Option<Vec<String>> {
        let (application, user, host) = Self::domain();
        // CopyValue 只查给定的域；CopyAppValue 会回落到全局域，不能用。
        let value = CFPreferencesCopyValue(&CFString::from_str(key), application, user, host)?;
        let array = value.downcast_ref::<CFArray>()?;
        // SAFETY: CFArray 的元素都是 CF 对象；下面逐个检查是不是字符串。
        let array = unsafe { array.cast_unchecked::<CFType>() };
        array
            .iter()
            .map(|item| item.downcast_ref::<CFString>().map(ToString::to_string))
            .collect()
    }

    fn set(&self, key: &str, languages: &[String]) {
        let strings: Vec<_> = languages
            .iter()
            .map(|tag| CFString::from_str(tag))
            .collect();
        let array = CFArray::from_retained_objects(&strings);
        Self::write(key, Some(&array));
    }

    fn remove(&self, key: &str) {
        Self::write(key, None);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;
    use crate::bridge::UiError;
    use crate::shell_locale::settle;

    #[derive(Default)]
    struct Memory {
        values: RefCell<BTreeMap<String, Vec<String>>>,
        writes: RefCell<usize>,
    }

    impl Memory {
        fn with(entries: &[(&str, &[&str])]) -> Self {
            let store = Self::default();
            for (key, value) in entries {
                store
                    .values
                    .borrow_mut()
                    .insert((*key).into(), strings(value));
            }
            store
        }
        fn value(&self, key: &str) -> Option<Vec<String>> {
            self.values.borrow().get(key).cloned()
        }
        fn writes(&self) -> usize {
            *self.writes.borrow()
        }
    }

    impl AppLanguageStore for Memory {
        fn get(&self, key: &str) -> Option<Vec<String>> {
            self.value(key)
        }
        fn set(&self, key: &str, languages: &[String]) {
            *self.writes.borrow_mut() += 1;
            self.values
                .borrow_mut()
                .insert(key.into(), languages.to_vec());
        }
        fn remove(&self, key: &str) {
            *self.writes.borrow_mut() += 1;
            self.values.borrow_mut().remove(key);
        }
    }

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
    fn explicit_mode_writes_the_list_and_the_companion_once() {
        let store = Memory::default();
        let english = preference("explicit", &["en"], &["en"]);
        assert_eq!(mirror(&store, &english), Mirror::Written);
        assert_eq!(store.value(APPLE_LANGUAGES), Some(strings(&["en"])));
        assert_eq!(store.value(MIRRORED_KEY), Some(strings(&["en"])));
        assert_eq!(
            mirror(&store, &english),
            Mirror::Unchanged,
            "值没变不重复写"
        );
        assert_eq!(store.writes(), 2);

        let ordered = preference("explicit", &["zh-Hans", "en"], &["zh-Hans", "en"]);
        assert_eq!(mirror(&store, &ordered), Mirror::Written);
        assert_eq!(
            store.value(APPLE_LANGUAGES),
            Some(strings(&["zh-Hans", "en"]))
        );
        assert_eq!(store.value(MIRRORED_KEY), Some(strings(&["zh-Hans", "en"])));
    }

    #[test]
    fn explicit_mode_overwrites_a_per_app_language_from_system_settings() {
        // 已知、已接受的副作用：在我们的选择器里选显式语言，会覆盖系统设置里给本 app 设的语言。
        let store = Memory::with(&[(APPLE_LANGUAGES, &["ja"])]);
        assert_eq!(
            mirror(&store, &preference("explicit", &["en"], &["en"])),
            Mirror::Written
        );
        assert_eq!(store.value(APPLE_LANGUAGES), Some(strings(&["en"])));
        assert_eq!(store.value(MIRRORED_KEY), Some(strings(&["en"])));
    }

    #[test]
    fn system_mode_removes_only_the_value_we_wrote() {
        let store = Memory::with(&[(APPLE_LANGUAGES, &["en"]), (MIRRORED_KEY, &["en"])]);
        let system = preference("system", &[], &["zh-Hans-CN", "en-US"]);
        assert_eq!(mirror(&store, &system), Mirror::Removed);
        assert_eq!(
            store.value(APPLE_LANGUAGES),
            None,
            "跟随系统：删键，不写 effective"
        );
        assert_eq!(store.value(MIRRORED_KEY), None);
        assert_eq!(
            mirror(&store, &system),
            Mirror::Unchanged,
            "已经没有伴随键，不再写"
        );
        assert_eq!(store.writes(), 2);
    }

    #[test]
    fn system_mode_keeps_a_per_app_language_set_in_system_settings() {
        // 没有伴随键：值来自系统设置，原样保留，每次启动都不动。
        let store = Memory::with(&[(APPLE_LANGUAGES, &["ja"])]);
        let system = preference("system", &[], &["zh-Hans-CN"]);
        assert_eq!(mirror(&store, &system), Mirror::Unchanged);
        assert_eq!(mirror(&store, &system), Mirror::Unchanged);
        assert_eq!(store.value(APPLE_LANGUAGES), Some(strings(&["ja"])));
        assert_eq!(store.writes(), 0);
    }

    #[test]
    fn system_mode_clears_a_stale_companion_and_keeps_the_user_value() {
        // 我们写过 en，之后用户在系统设置里改成 ja：保留 ja，只清掉过期的伴随键。
        let store = Memory::with(&[(APPLE_LANGUAGES, &["ja"]), (MIRRORED_KEY, &["en"])]);
        let system = preference("system", &[], &["zh-Hans-CN"]);
        assert_eq!(mirror(&store, &system), Mirror::CompanionCleared);
        assert_eq!(store.value(APPLE_LANGUAGES), Some(strings(&["ja"])));
        assert_eq!(store.value(MIRRORED_KEY), None);
        assert_eq!(mirror(&store, &system), Mirror::Unchanged);

        // 用户在系统设置里把按 app 语言改回默认（键被删）：同样只清伴随键。
        let store = Memory::with(&[(MIRRORED_KEY, &["en"])]);
        assert_eq!(mirror(&store, &system), Mirror::CompanionCleared);
        assert_eq!(store.value(MIRRORED_KEY), None);
        assert_eq!(store.value(APPLE_LANGUAGES), None);
    }

    #[test]
    fn unknown_preference_shapes_leave_the_store_alone() {
        let store = Memory::with(&[(APPLE_LANGUAGES, &["en"]), (MIRRORED_KEY, &["en"])]);
        assert_eq!(
            mirror(&store, &preference("explicit", &[], &["en"])),
            Mirror::Skipped
        );
        assert_eq!(
            mirror(&store, &preference("future", &["en"], &["en"])),
            Mirror::Skipped
        );
        assert_eq!(store.writes(), 0);
    }

    #[test]
    fn the_pickers_own_write_syncs_even_when_effective_does_not_change() {
        // 跟随系统时 effective 恰好是 ["en"]；选择器写显式 ["en"]：effective 不变，daemon 不推送，
        // 状态泵也不会同步——只能靠 `ui_set_locale_preference` 成功后的这一次。
        let mut seen = Some(strings(&["en"]));
        let reply = preference("explicit", &["en"], &["en"]);
        assert!(!crate::locale_changed(
            &mut seen,
            Some(reply.effective.clone())
        ));

        // 写入成功后 `shell_locale::after_set` 经 `settle` 按应答镜像。
        let store = Memory::default();
        let outcome = std::cell::Cell::new(None);
        let mirror_reply = |reply| {
            settle(
                Ok(reply),
                |p| outcome.set(Some(mirror(&store, p))),
                Vec::new,
            )
            .unwrap()
        };
        mirror_reply(reply);
        assert_eq!(outcome.take(), Some(Mirror::Written));
        assert_eq!(store.value(APPLE_LANGUAGES), Some(strings(&["en"])));
        assert_eq!(store.value(MIRRORED_KEY), Some(strings(&["en"])));

        // 切回跟随系统：同样立即删掉我们写的值。
        mirror_reply(preference("system", &[], &["en"]));
        assert_eq!(outcome.take(), Some(Mirror::Removed));
        assert_eq!(store.value(APPLE_LANGUAGES), None);

        // 写入失败（拒绝、老 daemon）不动存储。
        let failed = Err(UiError::invalid("locale_unsupported", "x"));
        assert!(settle(failed, |p| outcome.set(Some(mirror(&store, p))), Vec::new).is_err());
        assert_eq!(outcome.take(), None);
        assert_eq!(store.writes(), 4);
    }

    /// 本进程的语言按 CF 的规则从假存储读：本域有 `AppleLanguages` 就是它，否则是全局列表。
    fn process_languages_of(store: &Memory, global: &[&str]) -> Vec<String> {
        store
            .value(APPLE_LANGUAGES)
            .unwrap_or_else(|| strings(global))
    }

    #[test]
    fn a_stale_mirrored_value_never_counts_as_the_per_app_choice() {
        // 上次运行时偏好是显式 en，我们镜像了 en；本 app 关着时别的客户端改回跟随系统（系统中文）。
        // 启动后先镜像（删掉我们写的 en）再读本进程语言：得到全局中文，而不是过期的 en。
        let store = Memory::with(&[(APPLE_LANGUAGES, &["en"]), (MIRRORED_KEY, &["en"])]);
        let system = preference("system", &[], &["zh-Hans-CN"]);
        let resolved = settle(
            Ok(system.clone()),
            |p| assert_eq!(mirror(&store, p), Mirror::Removed),
            || process_languages_of(&store, &["zh-Hans-CN"]),
        )
        .unwrap();
        assert_eq!(resolved.effective, strings(&["zh-Hans-CN"]));
        assert_eq!(store.value(APPLE_LANGUAGES), None);

        // 用户自己在系统设置里给本 app 设的英文（没有伴随键）：保留，并且我们的界面跟着它。
        let store = Memory::with(&[(APPLE_LANGUAGES, &["en"])]);
        let resolved = settle(
            Ok(system),
            |p| assert_eq!(mirror(&store, p), Mirror::Unchanged),
            || process_languages_of(&store, &["zh-Hans-CN"]),
        )
        .unwrap();
        assert_eq!(resolved.effective, strings(&["en"]));
        assert_eq!(store.writes(), 0, "跟随系统从不写按 app 语言");
    }
}
