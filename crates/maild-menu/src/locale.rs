//! 菜单栏文案的语言（ADR-0048 决策 6）。
//!
//! 来源顺序：能连上 daemon 时用它 `getLocalePreference` 应答里的 `effective` 列表；连不上、超时、
//! 旧 Agent 不认识该方法（`-32601`）或应答为空时，用本进程读到的系统首选语言
//! （`+[NSLocale preferredLanguages]`）。两条路径都交给 [`maild_l10n::negotiate`] 协商。
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use maild_l10n::Locale;

/// 来源顺序规则（纯函数）：daemon 给了非空 `effective` 就用它，否则读系统语言。
pub(crate) fn resolve(
    effective: Option<&[String]>,
    system: impl FnOnce() -> Vec<String>,
) -> Locale {
    match effective {
        Some(effective) if !effective.is_empty() => maild_l10n::negotiate(effective),
        _ => maild_l10n::negotiate(&system()),
    }
}

/// 最近一次协商出的语言；RPC 线程写、主线程读，跨线程共享。
#[derive(Clone, Debug)]
pub(crate) struct LatestLocale(Arc<AtomicU8>);

impl LatestLocale {
    pub(crate) fn new(locale: Locale) -> Self {
        Self(Arc::new(AtomicU8::new(index_of(locale))))
    }

    pub(crate) fn get(&self) -> Locale {
        Locale::ALL
            .get(usize::from(self.0.load(Ordering::Relaxed)))
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn set(&self, locale: Locale) {
        self.0.store(index_of(locale), Ordering::Relaxed);
    }
}

fn index_of(locale: Locale) -> u8 {
    Locale::ALL
        .iter()
        .position(|candidate| *candidate == locale)
        .and_then(|index| u8::try_from(index).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_preference_wins_and_os_languages_are_the_fallback() {
        let english_os = || vec!["en-US".to_string()];
        let chinese = ["zh-Hans-CN".to_string(), "en".to_string()];
        assert_eq!(resolve(Some(&chinese), english_os), Locale::ZhHans);
        assert_eq!(
            resolve(Some(&["en".to_string()]), || panic!("daemon answered")),
            Locale::En
        );
        // daemon 不可达或应答为空：按系统语言协商；系统也协商不出时回落简体中文。
        assert_eq!(resolve(None, english_os), Locale::En);
        assert_eq!(resolve(Some(&[]), english_os), Locale::En);
        assert_eq!(resolve(None, Vec::new), Locale::ZhHans);
        assert_eq!(resolve(None, || vec!["ja-JP".into()]), Locale::ZhHans);
    }

    #[test]
    fn latest_locale_is_shared_between_clones() {
        let latest = LatestLocale::new(Locale::ZhHans);
        let writer = latest.clone();
        writer.set(Locale::En);
        assert_eq!(latest.get(), Locale::En);
        writer.set(Locale::ZhHans);
        assert_eq!(latest.get(), Locale::ZhHans);
    }

    #[test]
    fn every_locale_has_distinct_menu_titles() {
        let zh = &Locale::ZhHans.messages().menu;
        let en = &Locale::En.messages().menu;
        assert_eq!(zh.open_inbox, "打开收件箱");
        assert_eq!((zh.quit)("Featherpost"), "退出 Featherpost");
        assert_ne!(zh.open_inbox, en.open_inbox);
        assert!((en.quit)("Featherpost").contains("Featherpost"));
    }
}
