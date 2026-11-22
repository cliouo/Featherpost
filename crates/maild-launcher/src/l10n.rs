//! Language of the native launcher and recovery dialogs (ADR-0048 decision 6).
//!
//! Source order: the daemon's `effective` list (`getLocalePreference`) whenever this process
//! talks to a reachable Agent, otherwise the OS preferred languages, both negotiated with
//! [`maild_l10n::negotiate`]. Until the daemon has been asked (for example an error shown before
//! any connection exists), the OS languages decide. Every dialog string is read through
//! [`text`] at the moment it is shown, so a refresh applies to the next dialog.
use std::sync::atomic::{AtomicU8, Ordering};

use maild_l10n::{Launcher, Locale};

const UNRESOLVED: u8 = u8::MAX;
static CURRENT: AtomicU8 = AtomicU8::new(UNRESOLVED);

/// Launcher strings in the current language.
pub(crate) fn text() -> &'static Launcher {
    &locale().messages().launcher
}

pub(crate) fn locale() -> Locale {
    match CURRENT.load(Ordering::Relaxed) {
        UNRESOLVED => {
            let locale = system_locale();
            store(locale);
            locale
        }
        index => Locale::ALL
            .get(usize::from(index))
            .copied()
            .unwrap_or_default(),
    }
}

/// Apply what the daemon answered: its `effective` list when it answered with one, otherwise
/// (unreachable, timed out, an Agent that predates `getLocalePreference`) the OS languages.
pub(crate) fn apply_daemon_preference(effective: Option<&[String]>) {
    store(resolve(effective, system_languages));
}

/// Pure source-order rule shared by every caller.
fn resolve(effective: Option<&[String]>, system: impl FnOnce() -> Vec<String>) -> Locale {
    match effective {
        Some(effective) if !effective.is_empty() => maild_l10n::negotiate(effective),
        _ => maild_l10n::negotiate(&system()),
    }
}

fn system_locale() -> Locale {
    maild_l10n::negotiate(&system_languages())
}

/// Same OS reading as the daemon's `system` mode, here read by the launcher process itself.
fn system_languages() -> Vec<String> {
    maild::locale_preference::system_preferred_languages()
}

fn store(locale: Locale) {
    let index = Locale::ALL
        .iter()
        .position(|candidate| *candidate == locale)
        .unwrap_or_default();
    CURRENT.store(index as u8, Ordering::Relaxed);
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
        // Unreachable daemon, or an empty answer: the OS languages decide.
        assert_eq!(resolve(None, english_os), Locale::En);
        assert_eq!(resolve(Some(&[]), english_os), Locale::En);
        assert_eq!(resolve(None, Vec::new), Locale::ZhHans);
        assert_eq!(resolve(None, || vec!["zh-Hant-TW".into()]), Locale::ZhHans);
    }

    #[test]
    fn applied_locale_selects_the_launcher_strings() {
        apply_daemon_preference(Some(&["en-GB".to_string()]));
        assert_eq!(locale(), Locale::En);
        assert_eq!(text().button_close, maild_l10n::EN.launcher.button_close);
        apply_daemon_preference(Some(&["zh-Hans".to_string()]));
        assert_eq!(locale(), Locale::ZhHans);
        assert_eq!(text().button_close, "关闭");
    }
}
