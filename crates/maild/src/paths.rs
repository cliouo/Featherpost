//! Production path resolution for file-backed plugins.
//!
//! Hermetic callers keep using [`crate::assemble`]. The daemon uses
//! [`assemble_with_paths`] so relative persistent paths are independent from its working directory.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cordis::Context;
use maild_paths::AppPaths;

use crate::accounts;
use crate::api::ApiConfig;
use crate::config::intercept_config;
use crate::external::{ExternalConfig, ExternalInstallRoot};
use crate::frontend::{FrontendConfig, FrontendInstallRoot};
use crate::loader::{plugin_config, Assembly, LoadError};
use crate::store::StoreConfig;

pub fn assemble_with_paths(
    ctx: &Context,
    toml_text: &str,
    app_paths: &AppPaths,
    legacy_cwd: Option<&Path>,
) -> Result<Assembly, LoadError> {
    assemble_with_paths_from(
        ctx,
        toml_text,
        app_paths,
        legacy_cwd,
        &app_paths.config_file,
    )
}

pub fn assemble_with_paths_from(
    ctx: &Context,
    toml_text: &str,
    app_paths: &AppPaths,
    legacy_cwd: Option<&Path>,
    config_path: &Path,
) -> Result<Assembly, LoadError> {
    accounts::validate_accounts(toml_text)?;
    let mut effective = ctx.clone();

    // 插件身份缝（ADR-0017）：注册表与活动配置同目录，任何模式都装配。
    // 加载失败时缝自身停摆（有界诊断），不阻断其余插件装配。
    effective.provide::<crate::plugin_identity::IdentitySeam>(
        crate::plugin_identity::IdentitySeam::open(
            config_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .join("plugin_auth.toml"),
        ),
    );

    // 阅读策略（ADR-0032 决策 5）：与活动配置同目录，任何模式都装配。文件损坏时服务自身
    // 停摆（读缺省、写拒绝、不覆盖），不阻断其余插件。
    effective.provide::<crate::reading_policy::ReadingPolicy>(Arc::new(
        crate::reading_policy::ReadingPolicy::open(crate::reading_policy::policy_path_for(
            config_path,
        )),
    ));

    // rules 侧车（ADR-0043）：与 `reading_policy.toml` 同目录、load 之前提供。只有
    // `[plugins.rules.config] persist = true` 时 `RulesPlugin` 才据此打开持久模式；
    // `setSettings` 的 `persist: true` 也只在这条路径存在时才能动态开启落盘。
    effective.provide::<crate::rules::RulesStatePath>(Arc::new(crate::rules::RulesStatePath(
        crate::rules::rules_state_path_for(config_path),
    )));

    if plugin_config::<FrontendConfig>(toml_text, "frontend")?.is_some() {
        if ensure_managed_parent(&app_paths.data_dir, &app_paths.frontend_dir).is_err() {
            eprintln!("[maild/frontend] 前端目录不可安全准备，将仅提供原生修复入口");
            effective.provide(Arc::new(crate::frontend::FrontendDirectoryUnavailable));
        } else {
            effective.provide::<FrontendInstallRoot>(Arc::new(FrontendInstallRoot(
                app_paths.frontend_dir.clone(),
            )));
        }
        if app_paths.manages_accounts(config_path) {
            match crate::recovery::BundledFrontend::discover() {
                Ok(Some(source)) => effective.provide(Arc::new(source)),
                Ok(None) => {},
                Err(_) => eprintln!("[maild/frontend] 内置恢复副本不可用"),
            }
        }
    }

    if plugin_config::<ExternalConfig>(toml_text, "external")?.is_some() {
        ensure_managed_parent(&app_paths.data_dir, &app_paths.plugin_dir)?;
        ensure_managed_parent(&app_paths.data_dir, &app_paths.plugin_data_dir)?;
        effective.provide::<ExternalInstallRoot>(Arc::new(ExternalInstallRoot {
            root: app_paths.plugin_dir.clone(),
            data_root: app_paths.plugin_data_dir.clone(),
            config_path: config_path.to_path_buf(),
        }));
    }

    if let Some(mut store) = plugin_config::<StoreConfig>(toml_text, "store")? {
        if !store.db_path.is_empty() {
            let configured = Path::new(&store.db_path);
            if configured.is_relative() {
                let resolved = resolve_store_path(&store, app_paths)?
                    .expect("non-empty relative store path resolves");
                if let Some(cwd) = legacy_cwd {
                    check_legacy_database_conflict(configured, &resolved, cwd)?;
                }
                ensure_managed_parent(
                    &app_paths.data_dir,
                    resolved.parent().ok_or_else(|| {
                        LoadError::Structure(format!("数据库路径缺少父目录: {resolved:?}"))
                    })?,
                )?;
                store.db_path = path_to_config_string(&resolved)?;
            }
        }
        effective = intercept_config(&effective, store);
    }

    if let Some(mut api) = plugin_config::<ApiConfig>(toml_text, "api")? {
        if uses_unix_path(&api.transport) {
            let configured = Path::new(&api.listen);
            if configured.is_relative() {
                let resolved =
                    resolve_api_path(&api, app_paths)?.expect("relative Unix API path resolves");
                if let Some(cwd) = legacy_cwd {
                    check_legacy_socket_conflict(configured, &resolved, cwd)?;
                }
                ensure_managed_parent(
                    &app_paths.runtime_dir,
                    resolved.parent().ok_or_else(|| {
                        LoadError::Structure(format!("Unix socket 路径缺少父目录: {resolved:?}"))
                    })?,
                )?;
                api.listen = path_to_config_string(&resolved)?;
            }
        }
        effective = intercept_config(&effective, api);
    }

    // 受管账户目录：仅平台默认配置时开放写入口（ADR-0015 §1）。
    let managed_accounts = if app_paths.manages_accounts(config_path) {
        Some(crate::accounts::ManagedAccountsOptions {
            catalog_path: app_paths.accounts_file.clone(),
        })
    } else {
        None
    };

    crate::accounts::assemble_with_options(&effective, toml_text, managed_accounts)
}

fn path_to_config_string(path: &Path) -> Result<String, LoadError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        LoadError::Structure(format!("平台路径不能表示为 TOML UTF-8 字符串: {:?}", path))
    })
}

pub fn resolve_store_path(
    config: &StoreConfig,
    app_paths: &AppPaths,
) -> Result<Option<PathBuf>, LoadError> {
    if config.db_path.is_empty() {
        Ok(None)
    } else {
        app_paths
            .database_path(Path::new(&config.db_path))
            .map(Some)
            .map_err(paths_error)
    }
}

pub fn resolve_api_path(
    config: &ApiConfig,
    app_paths: &AppPaths,
) -> Result<Option<PathBuf>, LoadError> {
    if uses_unix_path(&config.transport) {
        app_paths
            .runtime_path(Path::new(&config.listen))
            .map(Some)
            .map_err(paths_error)
    } else {
        Ok(None)
    }
}

fn paths_error(error: maild_paths::PathsError) -> LoadError {
    LoadError::Structure(format!("平台路径配置错误: {error}"))
}

fn uses_unix_path(transport: &str) -> bool {
    #[cfg(unix)]
    {
        matches!(transport, "unix" | "unix_socket" | "socket" | "ipc")
    }
    #[cfg(not(unix))]
    {
        matches!(transport, "unix" | "unix_socket" | "socket")
    }
}

fn check_legacy_database_conflict(
    configured: &Path,
    resolved: &Path,
    cwd: &Path,
) -> Result<(), LoadError> {
    let legacy = cwd.join(configured);
    if same_lexical_path(&legacy, resolved) {
        return Ok(());
    }

    let mut candidates = vec![legacy.clone()];
    candidates.push(append_suffix(&legacy, "-wal"));
    candidates.push(append_suffix(&legacy, "-shm"));
    candidates.push(append_suffix(&legacy, "-journal"));
    if let (Some(parent), Some(name)) = (legacy.parent(), legacy.file_name()) {
        let mut lock_name = OsString::from(".");
        lock_name.push(name);
        lock_name.push(".maild.lock");
        candidates.push(parent.join(lock_name));
    }
    if candidates
        .iter()
        .map(|path| legacy_path_conflicts(cwd, path))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|exists| exists)
    {
        return Err(LoadError::Structure(format!(
            "相对 store.db_path 的旧工作目录数据仍存在于 {legacy:?}；请把旧路径写成绝对路径继续使用，或停服后通过 backup/restore 显式迁移到 {resolved:?}"
        )));
    }
    Ok(())
}

fn check_legacy_socket_conflict(
    configured: &Path,
    resolved: &Path,
    cwd: &Path,
) -> Result<(), LoadError> {
    let legacy = cwd.join(configured);
    if !same_lexical_path(&legacy, resolved) && legacy_path_conflicts(cwd, &legacy)? {
        return Err(LoadError::Structure(format!(
            "相对 api.listen 的旧工作目录 socket 仍存在于 {legacy:?}；确认旧进程退出并显式处理后再使用平台路径 {resolved:?}"
        )));
    }
    Ok(())
}

fn same_lexical_path(left: &Path, right: &Path) -> bool {
    left == right
}

fn ensure_managed_parent(root: &Path, parent: &Path) -> Result<(), LoadError> {
    ensure_private_directory(root)?;
    let relative = parent.strip_prefix(root).map_err(|_| {
        LoadError::Structure(format!("受管路径父目录 {parent:?} 不在平台根 {root:?} 内"))
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        ensure_private_directory(&current)?;
    }
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn legacy_path_conflicts(root: &Path, path: &Path) -> Result<bool, LoadError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        LoadError::Structure(format!("旧工作目录候选 {path:?} 不在工作目录 {root:?} 内"))
    })?;
    let mut current = root.to_path_buf();
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                let is_final = index + 1 == component_count;
                if is_final || metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Ok(true);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(path_error(&current, error)),
        }
    }
    Ok(false)
}

fn ensure_private_directory(path: &Path) -> Result<(), LoadError> {
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(path)
                .map_err(|error| path_error(path, error))?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(path).map_err(|error| path_error(path, error))?;
    }

    let metadata = fs::symlink_metadata(path).map_err(|error| path_error(path, error))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(LoadError::Structure(format!(
            "平台目录必须是非符号链接目录: {path:?}"
        )));
    }
    #[cfg(unix)]
    validate_private_unix_directory(path, &metadata)?;
    Ok(())
}

fn path_error(path: &Path, error: std::io::Error) -> LoadError {
    LoadError::Structure(format!("准备平台目录 {path:?} 失败: {error}"))
}

#[cfg(unix)]
fn validate_private_unix_directory(path: &Path, metadata: &fs::Metadata) -> Result<(), LoadError> {
    use std::os::unix::fs::MetadataExt as _;

    let current_uid = unsafe { libc::geteuid() };
    if metadata.uid() != current_uid || metadata.mode() & 0o077 != 0 {
        return Err(LoadError::Structure(format!(
            "平台目录必须由当前 uid 所有且权限不宽于 0700: {path:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::api::ApiConfig;
    use crate::store::StoreConfig;

    fn paths(root: &Path) -> AppPaths {
        AppPaths::from_roots(
            &root.join("config"),
            &root.join("data"),
            Some(&root.join("runtime")),
            Some(&root.join("state")),
            root,
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn relative_store_and_socket_use_managed_roots() {
        let directory = tempfile::tempdir().unwrap();
        let app_paths = paths(directory.path());
        let legacy = directory.path().join("legacy");
        fs::create_dir(&legacy).unwrap();
        let ctx = Context::new();
        let mut assembly = assemble_with_paths(
            &ctx,
            r#"
                [plugins.store.config]
                db_path = "mail/mail.sqlite3"
                [plugins.api.config]
                transport = "unix"
                listen = "rpc/mail.sock"
            "#,
            &app_paths,
            Some(&legacy),
        )
        .unwrap();

        assert_eq!(
            assembly.ctx.get::<StoreConfig>().unwrap().db_path,
            app_paths
                .data_dir
                .join("mail/mail.sqlite3")
                .to_string_lossy()
        );
        assert!(app_paths.data_dir.join("mail/mail.sqlite3").exists());
        assert_eq!(
            assembly.ctx.get::<ApiConfig>().unwrap().listen,
            app_paths
                .runtime_dir
                .join("rpc/mail.sock")
                .to_string_lossy()
        );
        assert!(app_paths.runtime_dir.join("rpc/mail.sock").exists());
        assembly.dispose().await;
        assert!(!app_paths.runtime_dir.join("rpc/mail.sock").exists());
    }

    #[tokio::test]
    async fn external_packages_and_private_data_use_separate_managed_roots() {
        let directory = tempfile::tempdir().unwrap();
        let app_paths = paths(directory.path());
        let mut assembly = assemble_with_paths(
            &Context::new(),
            "[plugins.external.config]\n",
            &app_paths,
            None,
        )
        .unwrap();

        let roots = assembly.ctx.get::<ExternalInstallRoot>().unwrap();
        assert_eq!(roots.root, app_paths.plugin_dir);
        assert_eq!(roots.data_root, app_paths.plugin_data_dir);
        assert!(roots.root.is_dir());
        assert!(roots.data_root.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&roots.data_root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }

        assembly.dispose().await;
    }

    #[test]
    fn path_resolution_does_not_rewrite_http_or_named_pipe() {
        let directory = tempfile::tempdir().unwrap();
        let app_paths = paths(directory.path());

        for (transport, listen) in [
            ("http", "127.0.0.1:7394"),
            ("named_pipe", r"\\.\pipe\maild"),
        ] {
            let config = ApiConfig {
                transport: transport.into(),
                listen: listen.into(),
                bearer_token: None,
            };
            assert!(resolve_api_path(&config, &app_paths).unwrap().is_none());
        }
    }

    #[test]
    fn legacy_database_artifacts_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let app_paths = paths(directory.path());
        let legacy = directory.path().join("legacy");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("mail.sqlite3-wal"), b"legacy").unwrap();
        let error = match assemble_with_paths(
            &Context::new(),
            "[plugins.store.config]\ndb_path = 'mail.sqlite3'\n",
            &app_paths,
            Some(&legacy),
        ) {
            Ok(_) => panic!("legacy conflict must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("旧工作目录数据"));
        assert!(!app_paths.database_file.exists());
    }

    #[test]
    fn managed_relative_paths_cannot_escape_their_roots() {
        let directory = tempfile::tempdir().unwrap();
        let app_paths = paths(directory.path());
        let error = match assemble_with_paths(
            &Context::new(),
            "[plugins.store.config]\ndb_path = '../outside.sqlite3'\n",
            &app_paths,
            None,
        ) {
            Ok(_) => panic!("parent traversal must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("不能包含父目录"));
        assert!(!directory.path().join("outside.sqlite3").exists());
    }

    #[test]
    fn invalid_notification_policy_is_rejected_before_managed_resources_exist() {
        let directory = tempfile::tempdir().unwrap();
        let app_paths = paths(directory.path());
        let error = match assemble_with_paths(
            &Context::new(),
            r#"
                [plugins.store.config]
                db_path = "mail/mail.sqlite3"
                [plugins.api.config]
                transport = "unix"
                listen = "rpc/mail.sock"
                [plugins.rules.config]
                quiet_hours = [{ start = "22:00", end = "22:00" }]
            "#,
            &app_paths,
            None,
        ) {
            Ok(_) => panic!("invalid rules must fail before plugin assembly"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("start 和 end 不能相同"));
        assert!(!app_paths.data_dir.exists());
        assert!(!app_paths.runtime_dir.exists());

        let error = match assemble_with_paths(
            &Context::new(),
            r#"
                [plugins.store.config]
                db_path = "mail/mail.sqlite3"
                [plugins.notify.config]
                provider = "osascript"
            "#,
            &app_paths,
            None,
        ) {
            Ok(_) => panic!("unknown notifier must fail before plugin assembly"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("notify provider 不支持"));
        assert!(!app_paths.data_dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_legacy_artifact_is_still_a_conflict() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let app_paths = paths(directory.path());
        let legacy = directory.path().join("legacy");
        fs::create_dir(&legacy).unwrap();
        symlink("missing-target", legacy.join("old-data")).unwrap();
        let error = match assemble_with_paths(
            &Context::new(),
            "[plugins.store.config]\ndb_path = 'old-data/mail.sqlite3'\n",
            &app_paths,
            Some(&legacy),
        ) {
            Ok(_) => panic!("dangling legacy artifact must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("旧工作目录数据"));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_managed_directory_and_legacy_socket_fail_closed() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let app_paths = paths(directory.path());
        let legacy = directory.path().join("legacy");
        fs::create_dir(&legacy).unwrap();
        fs::create_dir(&app_paths.data_dir).unwrap();
        fs::set_permissions(&app_paths.data_dir, fs::Permissions::from_mode(0o777)).unwrap();
        let error = match assemble_with_paths(
            &Context::new(),
            "[plugins.store.config]\ndb_path = 'nested/mail.sqlite3'\n",
            &app_paths,
            Some(&legacy),
        ) {
            Ok(_) => panic!("unsafe data directory must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("权限不宽于 0700"));
        assert!(!app_paths.data_dir.join("nested/mail.sqlite3").exists());

        let socket_directory = tempfile::tempdir().unwrap();
        let socket_paths = paths(socket_directory.path());
        let socket_legacy = socket_directory.path().join("legacy");
        fs::create_dir(&socket_legacy).unwrap();
        fs::create_dir(&socket_paths.runtime_dir).unwrap();
        fs::set_permissions(&socket_paths.runtime_dir, fs::Permissions::from_mode(0o777)).unwrap();
        let error = match assemble_with_paths(
            &Context::new(),
            "[plugins.api.config]\ntransport = 'unix'\nlisten = 'nested/mail.sock'\n",
            &socket_paths,
            Some(&socket_legacy),
        ) {
            Ok(_) => panic!("unsafe runtime root must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("权限不宽于 0700"));
        assert!(!socket_paths.runtime_dir.join("nested/mail.sock").exists());

        fs::write(legacy.join("mail.sock"), b"stale").unwrap();
        let error = match assemble_with_paths(
            &Context::new(),
            "[plugins.api.config]\ntransport = 'unix'\nlisten = 'mail.sock'\n",
            &app_paths,
            Some(&legacy),
        ) {
            Ok(_) => panic!("legacy socket must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("旧工作目录 socket"));
    }

    #[test]
    fn intercept_uses_resolved_configs() {
        let ctx = Context::new();
        let store = StoreConfig {
            db_path: "/tmp/resolved.sqlite3".into(),
        };
        let intercepted = intercept_config(&ctx, store.clone());
        intercepted.provide::<StoreConfig>(Arc::new(StoreConfig {
            db_path: "relative.sqlite3".into(),
        }));
        assert_eq!(
            intercepted.get::<StoreConfig>().unwrap().db_path,
            store.db_path
        );
    }
}
