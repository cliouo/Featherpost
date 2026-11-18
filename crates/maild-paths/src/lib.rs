//! Platform path and configuration discovery contract shared by every Featherpost process.
//!
//! This crate computes paths only. It never creates, migrates, removes or rewrites user data.

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};

pub use maild_brand::{CONFIG_ENV, CONFIG_FILE_NAME, PLUGIN_DATA_ENV};
/// 平台目录名（`brand.toml` 的 `slug`）。
pub const APP_NAME: &str = maild_brand::SLUG;
/// 受管账户目录（ADR-0015）：仅非秘密配置，秘密存平台凭据库。
pub const ACCOUNTS_FILE_NAME: &str = "accounts.toml";
pub use maild_brand::{DATABASE_FILE_NAME, SOCKET_FILE_NAME};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub log_dir: PathBuf,
    pub config_file: PathBuf,
    pub accounts_file: PathBuf,
    pub database_file: PathBuf,
    pub socket_file: PathBuf,
    pub backup_dir: PathBuf,
    pub release_dir: PathBuf,
    pub frontend_dir: PathBuf,
    pub plugin_dir: PathBuf,
    /// Stable host-owned data root for external plugins, separate from replaceable packages.
    pub plugin_data_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self, PathsError> {
        let project = ProjectDirs::from("", "", APP_NAME).ok_or(PathsError::HomeUnavailable)?;
        let base = BaseDirs::new().ok_or(PathsError::HomeUnavailable)?;
        Ok(Self::from_roots(
            project.config_local_dir(),
            project.data_local_dir(),
            project.runtime_dir(),
            project.state_dir(),
            base.home_dir(),
        ))
    }

    pub fn from_roots(
        config_dir: &Path,
        data_dir: &Path,
        runtime_dir: Option<&Path>,
        _state_dir: Option<&Path>,
        _home_dir: &Path,
    ) -> Self {
        let config_dir = config_dir.to_path_buf();
        let data_dir = data_dir.to_path_buf();
        let runtime_dir = runtime_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| data_dir.join("run"));
        #[cfg(target_os = "macos")]
        let log_dir = _home_dir.join("Library").join("Logs").join(APP_NAME);
        #[cfg(not(target_os = "macos"))]
        let log_dir = _state_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| data_dir.clone())
            .join("logs");

        Self {
            config_file: config_dir.join(CONFIG_FILE_NAME),
            accounts_file: config_dir.join(ACCOUNTS_FILE_NAME),
            database_file: data_dir.join(DATABASE_FILE_NAME),
            socket_file: runtime_dir.join(SOCKET_FILE_NAME),
            backup_dir: data_dir.join("backups"),
            release_dir: data_dir.join("releases"),
            frontend_dir: data_dir.join("frontends"),
            plugin_dir: data_dir.join("plugins"),
            plugin_data_dir: data_dir.join("plugin-data"),
            config_dir,
            data_dir,
            runtime_dir,
            log_dir,
        }
    }

    pub fn database_path(&self, configured: &Path) -> Result<PathBuf, PathsError> {
        resolve_managed_path(configured, &self.data_dir)
    }

    /// 受管账户目录是否归本进程写入（ADR-0015 §1）。
    ///
    /// 只有 Agent 使用平台默认配置（`config_file` 本身）时才开放；显式 `--config`、
    /// `<ENV_PREFIX>_CONFIG` 或开发态 bare daemon 继续使用旧 `[[accounts]]`，不隐式选择新位置。
    pub fn manages_accounts(&self, resolved_config: &Path) -> bool {
        resolved_config == self.config_file
    }

    pub fn runtime_path(&self, configured: &Path) -> Result<PathBuf, PathsError> {
        resolve_managed_path(configured, &self.runtime_dir)
    }

    #[cfg(test)]
    fn fixture(root: &Path) -> Self {
        Self::from_roots(
            &root.join("config"),
            &root.join("data"),
            Some(&root.join("runtime")),
            Some(&root.join("state")),
            root,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigSource {
    Explicit,
    Environment,
    Platform,
    LegacyWorkingDirectory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedConfig {
    pub path: PathBuf,
    pub source: ConfigSource,
}

impl ResolvedConfig {
    pub fn parent(&self) -> Result<&Path, PathsError> {
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| PathsError::InvalidConfigPath(self.path.clone()))
    }
}

#[derive(Debug)]
pub enum PathsError {
    HomeUnavailable,
    CurrentDirectory(std::io::Error),
    EmptyConfigEnvironment,
    RelativeConfigEnvironment(PathBuf),
    InvalidConfigPath(PathBuf),
    UnsafeRelativePath(PathBuf),
    AmbiguousConfig {
        platform: PathBuf,
        legacy: PathBuf,
    },
    InspectConfig {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for PathsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HomeUnavailable => f.write_str("无法确定当前用户的平台目录"),
            Self::CurrentDirectory(error) => write!(f, "无法确定当前工作目录: {error}"),
            Self::EmptyConfigEnvironment => write!(f, "{CONFIG_ENV} 不能为空"),
            Self::RelativeConfigEnvironment(path) => {
                write!(f, "{CONFIG_ENV} 必须是绝对路径，收到: {path:?}")
            }
            Self::InvalidConfigPath(path) => {
                write!(f, "配置路径缺少有效父目录: {path:?}")
            }
            Self::UnsafeRelativePath(path) => {
                write!(f, "受管相对路径不能包含父目录或平台前缀: {path:?}")
            }
            Self::AmbiguousConfig { platform, legacy } => write!(
                f,
                "平台配置 {platform:?} 与当前目录配置 {legacy:?} 同时存在；请用 --config 明确选择"
            ),
            Self::InspectConfig { path, source } => {
                write!(f, "检查配置路径 {path:?} 失败: {source}")
            }
        }
    }
}

impl std::error::Error for PathsError {}

pub fn resolve_config(explicit: Option<&Path>) -> Result<ResolvedConfig, PathsError> {
    if let Some(path) = explicit {
        if path.is_absolute() {
            return Ok(ResolvedConfig {
                path: path.to_path_buf(),
                source: ConfigSource::Explicit,
            });
        }
        let cwd = env::current_dir().map_err(PathsError::CurrentDirectory)?;
        return Ok(ResolvedConfig {
            path: absolutize(path, &cwd),
            source: ConfigSource::Explicit,
        });
    }
    let environment = env::var_os(CONFIG_ENV);
    if let Some(value) = environment.as_ref() {
        if value.is_empty() {
            return Err(PathsError::EmptyConfigEnvironment);
        }
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(PathsError::RelativeConfigEnvironment(path));
        }
        return Ok(ResolvedConfig {
            path,
            source: ConfigSource::Environment,
        });
    }
    let paths = AppPaths::discover()?;
    let platform_exists = path_entry_exists(&paths.config_file)?;
    let legacy = env::current_dir()
        .ok()
        .map(|cwd| cwd.join(CONFIG_FILE_NAME));
    let legacy_exists = match legacy.as_ref() {
        Some(path) => path_entry_exists(path)?,
        None => false,
    };
    if platform_exists && legacy_exists {
        let legacy = legacy.expect("legacy path exists");
        if same_config_entry(&paths.config_file, &legacy)? {
            return Ok(ResolvedConfig {
                path: paths.config_file,
                source: ConfigSource::Platform,
            });
        }
        return Err(PathsError::AmbiguousConfig {
            platform: paths.config_file,
            legacy,
        });
    }
    if platform_exists {
        return Ok(ResolvedConfig {
            path: paths.config_file,
            source: ConfigSource::Platform,
        });
    }
    if legacy_exists {
        return Ok(ResolvedConfig {
            path: legacy.expect("legacy path exists"),
            source: ConfigSource::LegacyWorkingDirectory,
        });
    }
    Ok(ResolvedConfig {
        path: paths.config_file,
        source: ConfigSource::Platform,
    })
}

#[cfg(test)]
fn resolve_config_with(
    explicit: Option<&Path>,
    environment: Option<std::ffi::OsString>,
    paths: &AppPaths,
    cwd: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Result<ResolvedConfig, PathsError> {
    if let Some(path) = explicit {
        return Ok(ResolvedConfig {
            path: absolutize(path, cwd),
            source: ConfigSource::Explicit,
        });
    }

    if let Some(value) = environment {
        if value.is_empty() {
            return Err(PathsError::EmptyConfigEnvironment);
        }
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(PathsError::RelativeConfigEnvironment(path));
        }
        return Ok(ResolvedConfig {
            path,
            source: ConfigSource::Environment,
        });
    }

    let platform_exists = exists(&paths.config_file);
    let legacy = cwd.join(CONFIG_FILE_NAME);
    let legacy_exists = exists(&legacy);
    if platform_exists && legacy_exists {
        if paths.config_file == legacy {
            return Ok(ResolvedConfig {
                path: paths.config_file.clone(),
                source: ConfigSource::Platform,
            });
        }
        return Err(PathsError::AmbiguousConfig {
            platform: paths.config_file.clone(),
            legacy,
        });
    }
    if platform_exists {
        return Ok(ResolvedConfig {
            path: paths.config_file.clone(),
            source: ConfigSource::Platform,
        });
    }

    if legacy_exists {
        return Ok(ResolvedConfig {
            path: legacy,
            source: ConfigSource::LegacyWorkingDirectory,
        });
    }

    Ok(ResolvedConfig {
        path: paths.config_file.clone(),
        source: ConfigSource::Platform,
    })
}

pub fn resolve_relative(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn resolve_managed_path(path: &Path, base: &Path) -> Result<PathBuf, PathsError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PathsError::UnsafeRelativePath(path.to_path_buf()))
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(PathsError::UnsafeRelativePath(path.to_path_buf()));
    }
    Ok(base.join(normalized))
}

fn path_entry_exists(path: &Path) -> Result<bool, PathsError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(PathsError::InspectConfig {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn same_config_entry(left: &Path, right: &Path) -> Result<bool, PathsError> {
    if left == right {
        return Ok(true);
    }
    let left = std::fs::canonicalize(left).map_err(|source| PathsError::InspectConfig {
        path: left.to_path_buf(),
        source,
    })?;
    let right = std::fs::canonicalize(right).map_err(|source| PathsError::InspectConfig {
        path: right.to_path_buf(),
        source,
    })?;
    Ok(left == right)
}

pub fn config_environment_is_set() -> bool {
    env::var_os(CONFIG_ENV).is_some()
}

fn absolutize(path: &Path, cwd: &Path) -> PathBuf {
    resolve_relative(path, cwd)
}

pub fn display_source(source: ConfigSource) -> &'static str {
    match source {
        ConfigSource::Explicit => "explicit",
        ConfigSource::Environment => "environment",
        ConfigSource::Platform => "platform",
        ConfigSource::LegacyWorkingDirectory => "legacy-cwd",
    }
}

pub fn is_reserved_config_name(value: &OsStr) -> bool {
    value == OsStr::new("backup") || value == OsStr::new("restore") || value == OsStr::new("paths")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn config_precedence_is_explicit_environment_then_unambiguous_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::fixture(dir.path());
        let cwd = dir.path().join("cwd");
        let explicit = dir.path().join("explicit.toml");
        let environment = dir.path().join("environment.toml");

        let result = resolve_config_with(
            Some(&explicit),
            Some(environment.clone().into_os_string()),
            &paths,
            &cwd,
            |_| true,
        )
        .unwrap();
        assert_eq!(result.path, explicit);
        assert_eq!(result.source, ConfigSource::Explicit);

        let result = resolve_config_with(
            None,
            Some(environment.clone().into_os_string()),
            &paths,
            &cwd,
            |_| true,
        )
        .unwrap();
        assert_eq!(result.path, environment);
        assert_eq!(result.source, ConfigSource::Environment);

        let result =
            resolve_config_with(None, None, &paths, &cwd, |path| path == paths.config_file)
                .unwrap();
        assert_eq!(result.path, paths.config_file);
        assert_eq!(result.source, ConfigSource::Platform);

        let legacy = cwd.join(CONFIG_FILE_NAME);
        let result = resolve_config_with(None, None, &paths, &cwd, |path| path == legacy).unwrap();
        assert_eq!(result.path, legacy);
        assert_eq!(result.source, ConfigSource::LegacyWorkingDirectory);
    }

    #[test]
    fn platform_and_legacy_configs_require_an_explicit_choice() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::fixture(dir.path());
        let cwd = dir.path().join("cwd");
        let legacy = cwd.join(CONFIG_FILE_NAME);
        let error = resolve_config_with(None, None, &paths, &cwd, |path| {
            path == paths.config_file || path == legacy
        })
        .unwrap_err();
        assert!(matches!(error, PathsError::AmbiguousConfig { .. }));

        let same = resolve_config_with(None, None, &paths, &paths.config_dir, |_| true).unwrap();
        assert_eq!(same.path, paths.config_file);
        assert_eq!(same.source, ConfigSource::Platform);

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            std::fs::create_dir_all(&paths.config_dir).unwrap();
            std::fs::write(&paths.config_file, "").unwrap();
            let alias_dir = dir.path().join("config-alias");
            symlink(&paths.config_dir, &alias_dir).unwrap();
            assert!(
                same_config_entry(&paths.config_file, &alias_dir.join(CONFIG_FILE_NAME)).unwrap()
            );
        }
    }

    #[test]
    fn missing_config_reports_platform_location_instead_of_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::fixture(dir.path());
        let result = resolve_config_with(None, None, &paths, dir.path(), |_| false).unwrap();
        assert_eq!(result.path, paths.config_file);
        assert_eq!(result.source, ConfigSource::Platform);
    }

    #[test]
    fn config_environment_must_be_absolute_and_non_empty() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::fixture(dir.path());
        assert!(matches!(
            resolve_config_with(None, Some(OsString::new()), &paths, dir.path(), |_| false),
            Err(PathsError::EmptyConfigEnvironment)
        ));
        assert!(matches!(
            resolve_config_with(
                None,
                Some(OsString::from("relative.toml")),
                &paths,
                dir.path(),
                |_| false
            ),
            Err(PathsError::RelativeConfigEnvironment(_))
        ));
    }

    #[test]
    fn relative_paths_are_anchored_without_touching_the_filesystem() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::fixture(root.path());
        assert_eq!(
            paths.database_path(Path::new(DATABASE_FILE_NAME)).unwrap(),
            paths.data_dir.join(DATABASE_FILE_NAME)
        );
        #[cfg(windows)]
        let absolute = PathBuf::from(r"C:\absolute").join(DATABASE_FILE_NAME);
        #[cfg(not(windows))]
        let absolute = PathBuf::from("/absolute").join(DATABASE_FILE_NAME);
        assert_eq!(paths.database_path(&absolute).unwrap(), absolute);
        assert!(matches!(
            paths.database_path(Path::new("../outside.sqlite3")),
            Err(PathsError::UnsafeRelativePath(_))
        ));
        assert!(matches!(
            paths.runtime_path(Path::new("nested/../../outside.sock")),
            Err(PathsError::UnsafeRelativePath(_))
        ));
    }

    #[test]
    fn missing_runtime_directory_falls_back_under_local_data() {
        let root = Path::new("/fixture");
        let paths =
            AppPaths::from_roots(&root.join("config"), &root.join("data"), None, None, root);
        assert_eq!(paths.runtime_dir, root.join("data").join("run"));
        assert_eq!(
            paths.socket_file,
            root.join("data").join("run").join(SOCKET_FILE_NAME)
        );
        assert_eq!(paths.frontend_dir, root.join("data").join("frontends"));
        assert_eq!(paths.plugin_dir, root.join("data").join("plugins"));
        assert_eq!(paths.plugin_data_dir, root.join("data").join("plugin-data"));
    }

    #[test]
    fn accounts_file_lives_next_to_the_platform_config_and_gates_writes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::fixture(dir.path());
        assert_eq!(
            paths.accounts_file,
            paths.config_dir.join(ACCOUNTS_FILE_NAME)
        );
        assert!(paths.manages_accounts(&paths.config_file));
        let explicit = dir.path().join("custom.toml");
        assert!(!paths.manages_accounts(&explicit));
    }

    #[test]
    fn brand_names_drive_every_discovered_location() {
        assert_eq!(APP_NAME, maild_brand::SLUG);
        assert_eq!(CONFIG_ENV, maild_brand::CONFIG_ENV);
        assert_eq!(PLUGIN_DATA_ENV, maild_brand::PLUGIN_DATA_ENV);
        assert_eq!(CONFIG_FILE_NAME, format!("{}.toml", maild_brand::SLUG));
        assert_eq!(DATABASE_FILE_NAME, format!("{}.sqlite3", maild_brand::SLUG));
        assert_eq!(SOCKET_FILE_NAME, format!("{}.sock", maild_brand::SLUG));

        let Ok(paths) = AppPaths::discover() else {
            return; // No home directory in this sandbox; nothing to discover.
        };
        assert!(paths
            .config_dir
            .components()
            .any(|part| part.as_os_str() == OsStr::new(APP_NAME)));
        assert_eq!(
            paths.config_file.file_name(),
            Some(OsStr::new(CONFIG_FILE_NAME))
        );
        assert_eq!(
            paths.database_file.file_name(),
            Some(OsStr::new(DATABASE_FILE_NAME))
        );
        assert_eq!(
            paths.socket_file.file_name(),
            Some(OsStr::new(SOCKET_FILE_NAME))
        );
        #[cfg(target_os = "macos")]
        {
            let support = Path::new("Library/Application Support").join(maild_brand::SLUG);
            assert!(paths
                .config_file
                .ends_with(support.join(maild_brand::CONFIG_FILE_NAME)));
            assert!(paths
                .database_file
                .ends_with(support.join(maild_brand::DATABASE_FILE_NAME)));
        }
    }
}
