//! Product brand constants: the Rust face of the repository's `brand.toml`.
//!
//! Everything user- or OS-visible that names the product (display name, CLI and binary names,
//! bundle names and identifiers, config/database/socket file names, environment variables,
//! the reader URI scheme, the Keychain service) comes from here, so a rename is
//! `edit brand.toml` + `bash scripts/sync-brand.sh` with no Rust source changes. Internal crate
//! names (`maild-*`) and the binary-scan marker prefixes intentionally stay stable.
//!
//! `generated.rs` is written by `scripts/sync-brand.sh`; never edit it by hand. The
//! `#[macro_export]` macros (`bin_exe_daemon!`, `agent_id_contract!`, …) expand to literals for
//! the places that need one at compile time (`env!`, `#[used]` statics).

mod generated;
pub use generated::*;

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[&str] = &[
        PRODUCT_NAME, SLUG, ENV_PREFIX, CLI_DAEMON, CLI_CTL, LAUNCHER_BIN, AGENT_BIN, MENU_BIN,
        UI_BIN, APP_BUNDLE_NAME, AGENT_BUNDLE_NAME, MENU_BUNDLE_NAME, UI_BUNDLE_NAME, ICON_NAME,
        APP_BUNDLE_ID, AGENT_BUNDLE_ID, MENU_BUNDLE_ID, UI_BUNDLE_ID, BUNDLED_FRONTEND_ID,
        URI_SCHEME, KEYCHAIN_SERVICE, CONFIG_FILE_NAME, DATABASE_FILE_NAME, SOCKET_FILE_NAME,
        CONFIG_ENV, SOCKET_ENV, URL_ENV, TOKEN_ENV, TOKEN_FILE_ENV, PLUGIN_TOKEN_ENV,
        PLUGIN_TOKEN_FILE_ENV, PLUGIN_DATA_ENV, MAINTENANCE_LOCK_FD_ENV, MAINTENANCE_VALIDATOR_ENV,
        BUNDLE_SHORT_VERSION_ENV, BUNDLE_BUILD_VERSION_ENV, UI_ORIGIN_ENV, AGENT_CONTRACT_PREFIX,
        MENU_CONTRACT_PREFIX, UI_EMBEDDED_CONTRACT_PREFIX,
    ];

    fn strip<'a>(value: &'a str, prefix: &str, suffix: &str) -> &'a str {
        value
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .unwrap_or_else(|| panic!("{value:?} is not {prefix:?}..{suffix:?}"))
    }

    #[test]
    fn every_constant_is_non_empty_and_trimmed() {
        for value in ALL {
            assert!(!value.is_empty());
            assert_eq!(value.trim(), *value);
            assert!(!value.contains('\0'));
        }
    }

    #[test]
    fn derived_names_follow_slug_and_env_prefix() {
        assert_eq!(strip(CONFIG_FILE_NAME, SLUG, ".toml"), "");
        assert_eq!(strip(DATABASE_FILE_NAME, SLUG, ".sqlite3"), "");
        assert_eq!(strip(SOCKET_FILE_NAME, SLUG, ".sock"), "");
        for (env, suffix) in [
            (CONFIG_ENV, "_CONFIG"),
            (SOCKET_ENV, "_SOCKET"),
            (URL_ENV, "_URL"),
            (TOKEN_ENV, "_TOKEN"),
            (TOKEN_FILE_ENV, "_TOKEN_FILE"),
            (PLUGIN_TOKEN_ENV, "_PLUGIN_TOKEN"),
            (PLUGIN_TOKEN_FILE_ENV, "_PLUGIN_TOKEN_FILE"),
            (PLUGIN_DATA_ENV, "_PLUGIN_DATA_DIR"),
            (MAINTENANCE_LOCK_FD_ENV, "_MAINTENANCE_LOCK_FD"),
            (MAINTENANCE_VALIDATOR_ENV, "_MAINTENANCE_VALIDATOR"),
            (BUNDLE_SHORT_VERSION_ENV, "_BUNDLE_SHORT_VERSION"),
            (BUNDLE_BUILD_VERSION_ENV, "_BUNDLE_BUILD_VERSION"),
            (UI_ORIGIN_ENV, "_UI_ORIGIN"),
        ] {
            assert_eq!(strip(env, ENV_PREFIX, suffix), "");
        }
        assert!(SLUG.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
        assert!(ENV_PREFIX.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'));
    }

    #[test]
    fn bundles_and_identifiers_are_consistent() {
        assert_eq!(BUNDLED_FRONTEND_ID, UI_BUNDLE_ID);
        for bundle in [APP_BUNDLE_NAME, AGENT_BUNDLE_NAME, MENU_BUNDLE_NAME, UI_BUNDLE_NAME] {
            assert!(bundle.ends_with(".app") && !bundle.contains('/'), "{bundle}");
        }
        let ids = [APP_BUNDLE_ID, AGENT_BUNDLE_ID, MENU_BUNDLE_ID, UI_BUNDLE_ID];
        for (i, id) in ids.iter().enumerate() {
            assert!(id.split('.').count() >= 2 && !id.split('.').any(str::is_empty), "{id}");
            assert!(!ids[i + 1..].contains(id), "bundle id {id} reused");
        }
        let bins = [CLI_DAEMON, CLI_CTL, LAUNCHER_BIN, AGENT_BIN, MENU_BIN, UI_BIN];
        for (i, bin) in bins.iter().enumerate() {
            assert!(!bins[i + 1..].contains(bin), "binary name {bin} reused");
        }
    }

    #[test]
    fn contract_macros_expand_to_nul_delimited_markers() {
        let agent: &str = crate::agent_id_contract!();
        let menu: &str = crate::menu_id_contract!();
        let ui: &str = crate::ui_embedded_contract!();
        assert_eq!(strip(strip(agent, "\0", "\0"), AGENT_CONTRACT_PREFIX, AGENT_BUNDLE_ID), "");
        assert_eq!(strip(strip(menu, "\0", "\0"), MENU_CONTRACT_PREFIX, MENU_BUNDLE_ID), "");
        assert_eq!(strip(strip(ui, "\0", "\0"), UI_EMBEDDED_CONTRACT_PREFIX, UI_BUNDLE_ID), "");
        // The macros must stay usable where only a literal is accepted.
        const _: &str = concat!(crate::agent_id_contract!(), crate::ui_embedded_contract!());
    }
}
