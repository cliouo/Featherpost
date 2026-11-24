//! Proxy configuration inheritance, per-plugin override storage, and `maild-proxy` integration
//! for the NodeSeek Email provider plugin.
//!
//! Reference implementation of the Cordis-style proxy model:
//! - Default is [`ProxyOverride::Inherit`] (zero-config: automatically follows Featherpost's
//!   global proxy settings fetched over local daemon IPC and hot-reloaded on `preferences/proxy`).
//! - Users can independently override `nodeseek-email` to [`ProxyOverride::Direct`] or
//!   [`ProxyOverride::Custom`] via the plugin UI or CLI, persisted in the plugin's private data
//!   directory (`proxy_override.json`, Unix `0600`, atomic write, fail-closed on corruption).
//! - Programmatic [`ProxyResolver`] hooks can also be chained above the plugin override.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use maild_client::{discover_endpoint, DiscoveryOptions, ProxyConfigResult, RpcClient};
use maild_proxy::reqwest_adapter::apply_proxy_with_resolver;
use maild_proxy::{
    detect_system_proxy, validate_custom_config, ChainedResolver, CustomProxyConfig, ProxyConfig,
    ProxyContextDefaults, ProxyEndpoint, ProxyMode, ProxyOverride, ProxyProtocol, ProxyResolver,
    ResolveOptions, SystemProxySnapshot,
};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult, ErrorCode};
use crate::PLUGIN_ID;

/// Persisted plugin-level proxy override filename inside the plugin's private data directory.
pub const PROXY_SETTINGS_FILE: &str = "proxy_override.json";
const FORMAT_VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProxySettingsFile {
    format_version: u32,
    #[serde(default, rename = "override")]
    proxy_override: ProxyOverride,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_custom: Option<CustomProxyConfig>,
}

/// Persisted plugin-level proxy override settings.
///
/// Defaults to [`ProxyOverride::Inherit`] ("Follow global proxy"). Switching temporarily to
/// `Inherit` or `Direct` preserves `saved_custom` so users do not lose their custom settings.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginProxySettings {
    #[serde(default, rename = "override")]
    pub proxy_override: ProxyOverride,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_custom: Option<CustomProxyConfig>,
}

impl fmt::Debug for PluginProxySettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginProxySettings")
            .field("proxy_override", &self.proxy_override)
            .field("saved_custom", &self.saved_custom)
            .finish()
    }
}

impl PluginProxySettings {
    pub fn inherit() -> Self {
        Self::default()
    }

    pub fn from_override(proxy_override: ProxyOverride) -> Self {
        let saved_custom = match &proxy_override {
            ProxyOverride::Custom(cfg) => Some(cfg.clone()),
            ProxyOverride::Inherit | ProxyOverride::Direct => None,
        };
        Self {
            proxy_override,
            saved_custom,
        }
    }

    pub fn effective_custom(&self) -> Option<&CustomProxyConfig> {
        match &self.proxy_override {
            ProxyOverride::Custom(cfg) => Some(cfg),
            ProxyOverride::Inherit | ProxyOverride::Direct => self.saved_custom.as_ref(),
        }
    }

    pub fn has_password(&self) -> bool {
        self.proxy_override.has_password()
            || self
                .saved_custom
                .as_ref()
                .is_some_and(CustomProxyConfig::has_password)
    }

    pub fn without_secret(&self) -> Self {
        Self {
            proxy_override: self.proxy_override.without_secret(),
            saved_custom: self
                .saved_custom
                .as_ref()
                .map(CustomProxyConfig::without_secret),
        }
    }

    pub fn validate(&self) -> AppResult<Self> {
        let proxy_override = self.proxy_override.validate().map_err(|detail| {
            AppError::with_detail(ErrorCode::InvalidInput, "插件代理覆盖配置无效", detail)
        })?;
        let saved_custom = self
            .saved_custom
            .as_ref()
            .map(validate_custom_config)
            .transpose()
            .map_err(|detail| {
                AppError::with_detail(ErrorCode::InvalidInput, "插件代理配置无效", detail)
            })?;
        Ok(Self {
            proxy_override,
            saved_custom,
        })
    }
}

/// Private file-backed store for [`PluginProxySettings`] (`0600`, atomic rename, fail-closed).
#[derive(Clone, Debug)]
pub struct PluginProxyStore {
    path: PathBuf,
}

impl PluginProxyStore {
    pub fn discover() -> AppResult<Self> {
        let root = match std::env::var_os(maild_paths::PLUGIN_DATA_ENV) {
            Some(value) if !value.is_empty() => {
                let value = PathBuf::from(value);
                if !value.is_absolute() {
                    return Err(AppError::state());
                }
                value
            }
            Some(_) => return Err(AppError::state()),
            None => maild_paths::AppPaths::discover()
                .map_err(|_| AppError::state())?
                .plugin_data_dir
                .join(PLUGIN_ID),
        };
        Self::at_root(root)
    }

    pub fn at_root(root: PathBuf) -> AppResult<Self> {
        ensure_private_directory(&root)?;
        Ok(Self {
            path: root.join(PROXY_SETTINGS_FILE),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Cheap metadata stamp used by long-lived processes to detect external override updates.
    pub fn file_stamp(&self) -> Option<(SystemTime, u64)> {
        let meta = fs::symlink_metadata(&self.path).ok()?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return None;
        }
        Some((meta.modified().ok()?, meta.len()))
    }

    /// Load settings from disk. Missing file returns default (`ProxyOverride::Inherit`) without
    /// creating any file on disk. Corrupt, future-version, symlink, or wide-mode files fail closed
    /// and are never overwritten.
    pub fn load(&self) -> AppResult<PluginProxySettings> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PluginProxySettings::default());
            }
            Err(_) => return Err(AppError::state()),
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(AppError::state());
        }
        check_private_file(&metadata)?;
        if metadata.len() > MAX_SETTINGS_BYTES {
            return Err(AppError::state());
        }
        let bytes = fs::read(&self.path).map_err(|_| AppError::state())?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(AppError::state());
        }
        let parsed: ProxySettingsFile =
            serde_json::from_slice(&bytes).map_err(|_| AppError::state())?;
        if parsed.format_version != FORMAT_VERSION {
            return Err(AppError::new(
                ErrorCode::State,
                "插件代理配置文件版本不受支持",
            ));
        }
        PluginProxySettings {
            proxy_override: parsed.proxy_override,
            saved_custom: parsed.saved_custom,
        }
        .validate()
    }

    /// Validate and atomically persist plugin proxy settings (`0600` temp file + rename).
    pub fn save(&self, settings: &PluginProxySettings) -> AppResult<PluginProxySettings> {
        // Fail closed if an existing corrupt/future file sits on disk: never overwrite it.
        if fs::symlink_metadata(&self.path).is_ok() {
            let _ = self.load()?;
        }
        let validated = settings.validate()?;
        let parent = self.path.parent().ok_or_else(AppError::state)?;
        ensure_private_directory(parent)?;

        let file = ProxySettingsFile {
            format_version: FORMAT_VERSION,
            proxy_override: validated.proxy_override.clone(),
            saved_custom: validated.saved_custom.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&file).map_err(|_| AppError::state())?;
        if encoded.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(AppError::invalid_input());
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_path = parent.join(format!(
            ".{PROXY_SETTINGS_FILE}.tmp-{}-{unique}",
            std::process::id()
        ));

        let write_result = (|| -> std::io::Result<()> {
            let mut opts = OpenOptions::new();
            opts.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                opts.mode(0o600);
            }
            let mut f = opts.open(&temp_path)?;
            f.write_all(&encoded)?;
            f.write_all(b"\n")?;
            f.sync_all()?;
            drop(f);
            fs::rename(&temp_path, &self.path)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
            return Err(AppError::state());
        }
        Ok(validated)
    }
}

/// Runtime proxy snapshot combining global daemon config, OS system snapshot, plugin-level
/// [`ProxyOverride`], and optional programmatic [`ProxyResolver`].
#[derive(Clone)]
pub struct ProxySnapshot {
    pub global_config: ProxyConfig,
    pub global_system: SystemProxySnapshot,
    pub plugin_override: ProxyOverride,
    pub extra_resolver: Option<Arc<dyn ProxyResolver>>,
    pub resolve_options: ResolveOptions,
}

impl fmt::Debug for ProxySnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxySnapshot")
            .field("global_config", &self.global_config)
            .field("global_system", &self.global_system)
            .field("plugin_override", &self.plugin_override)
            .field("has_extra_resolver", &self.extra_resolver.is_some())
            .finish()
    }
}

impl ProxySnapshot {
    /// Default zero-config snapshot: inherit global `System` mode + detected OS system proxy.
    pub fn default_system() -> Self {
        Self {
            global_config: ProxyConfig::default(),
            global_system: detect_system_proxy(),
            plugin_override: ProxyOverride::Inherit,
            extra_resolver: None,
            resolve_options: ResolveOptions::default(),
        }
    }

    pub fn from_global(global_config: ProxyConfig, global_system: SystemProxySnapshot) -> Self {
        Self {
            global_config,
            global_system,
            plugin_override: ProxyOverride::Inherit,
            extra_resolver: None,
            resolve_options: ResolveOptions::default(),
        }
    }

    pub fn with_override(mut self, plugin_override: ProxyOverride) -> Self {
        self.plugin_override = plugin_override;
        self
    }

    pub fn with_extra_resolver(mut self, resolver: Arc<dyn ProxyResolver>) -> Self {
        self.extra_resolver = Some(resolver);
        self
    }

    #[cfg(any(test, debug_assertions))]
    pub fn with_allow_loopback_proxy(mut self, allow: bool) -> Self {
        self.resolve_options.allow_loopback_proxy = allow;
        self
    }

    /// Attach Featherpost's `maild-proxy` policy (optional extra resolver -> plugin override ->
    /// global config -> system proxy) onto a `reqwest::ClientBuilder`.
    pub fn apply_reqwest_proxy(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        let mut chain = ChainedResolver::new().with_options(self.resolve_options);
        if let Some(extra) = &self.extra_resolver {
            chain.push_arc(100, Arc::clone(extra));
        }
        chain.push_with_priority(10, self.plugin_override.clone());
        apply_proxy_with_resolver(
            builder,
            self.global_config.clone(),
            self.global_system.clone(),
            Some(Arc::new(chain)),
            ProxyContextDefaults::plugin(PLUGIN_ID),
            self.resolve_options,
        )
    }
}

/// Convert `maild_client::ProxyConfigResult` into `(ProxyConfig, SystemProxySnapshot)`.
pub fn convert_client_proxy_result(
    result: &ProxyConfigResult,
) -> (ProxyConfig, SystemProxySnapshot) {
    convert_wire_proxy(&result.config, &result.effective_system)
}

/// Convert a `preferences/proxy` push notification into `(ProxyConfig, SystemProxySnapshot)`.
pub fn convert_proxy_event(
    event: &maild_client::ProxyPreferenceChanged,
) -> (ProxyConfig, SystemProxySnapshot) {
    convert_wire_proxy(&event.config, &event.effective_system)
}

/// Whether a `preferences/proxy` push notification requires a follow-up
/// `getProxyConfig { include_secret: true }` call to fetch omitted credentials.
pub fn event_requires_secret_fetch(event: &maild_client::ProxyPreferenceChanged) -> bool {
    if event.has_password {
        return true;
    }
    let sys = &event.effective_system;
    [&sys.http, &sys.https, &sys.socks5]
        .into_iter()
        .flatten()
        .any(|ep| ep.username.as_deref().is_some_and(|u| !u.is_empty()))
}

fn convert_wire_proxy(
    wire_cfg: &maild_client::ProxyConfig,
    wire_sys: &maild_client::SystemProxySnapshot,
) -> (ProxyConfig, SystemProxySnapshot) {
    let mode = match wire_cfg.mode.trim().to_ascii_lowercase().as_str() {
        "direct" => ProxyMode::Direct,
        "custom" => ProxyMode::Custom,
        _ => ProxyMode::System,
    };
    let custom = wire_cfg.custom.as_ref().and_then(|c| {
        let protocol = ProxyProtocol::parse_scheme(&c.protocol).ok()?;
        if c.host.trim().is_empty() || c.port == 0 {
            return None;
        }
        Some(CustomProxyConfig {
            protocol,
            host: c.host.clone(),
            port: c.port,
            username: c.username.clone(),
            password: c.password.clone(),
            bypass: c.bypass.clone(),
        })
    });
    let config = if mode == ProxyMode::Custom && custom.is_none() {
        ProxyConfig::default()
    } else {
        ProxyConfig { mode, custom }
    };

    let convert_ep = |ep: &maild_client::ProxyEndpointInfo| -> Option<ProxyEndpoint> {
        let protocol = ProxyProtocol::parse_scheme(&ep.protocol).ok()?;
        if ep.host.trim().is_empty() || ep.port == 0 {
            return None;
        }
        Some(ProxyEndpoint {
            protocol,
            host: ep.host.clone(),
            port: ep.port,
            username: ep.username.clone(),
            password: ep.password.clone(),
        })
    };
    let system = SystemProxySnapshot {
        http: wire_sys.http.as_ref().and_then(convert_ep),
        https: wire_sys.https.as_ref().and_then(convert_ep),
        socks5: wire_sys.socks5.as_ref().and_then(convert_ep),
        bypass: wire_sys.bypass.clone(),
        exclude_simple_hostnames: wire_sys.exclude_simple_hostnames,
        pac_url: wire_sys.pac_url.clone(),
        source: wire_sys.source.clone(),
    };
    (config, system)
}

/// Fetch global proxy config from the local daemon over stream IPC (`include_secret: true`),
/// falling back gracefully to `ProxyMode::System` + local OS detection if the daemon is offline
/// or an older version without `getProxyConfig`.
pub async fn fetch_global_proxy_from_daemon() -> (ProxyConfig, SystemProxySnapshot, bool) {
    let config_path = maild_paths::resolve_config(None).ok().map(|c| c.path);
    let Ok(endpoint) = discover_endpoint(&DiscoveryOptions {
        config_path,
        ..DiscoveryOptions::default()
    }) else {
        return (ProxyConfig::default(), detect_system_proxy(), false);
    };
    let Ok(client) = RpcClient::new(endpoint.clone()) else {
        return (ProxyConfig::default(), detect_system_proxy(), false);
    };
    let fetched = if endpoint.supports_notifications() {
        client.proxy_config_with_secret().await
    } else {
        client.proxy_config().await
    };
    match fetched {
        Ok(result) => {
            let (cfg, sys) = convert_client_proxy_result(&result);
            (cfg, sys, true)
        }
        Err(_) => (ProxyConfig::default(), detect_system_proxy(), false),
    }
}

/// Load effective [`ProxySnapshot`] for CLI / UI one-shot requests: global daemon config +
/// persisted plugin-level override (`Inherit` by default).
pub async fn load_effective_proxy_snapshot() -> ProxySnapshot {
    let (global_config, global_system, _) = fetch_global_proxy_from_daemon().await;
    let plugin_override = PluginProxyStore::discover()
        .and_then(|store| store.load())
        .map(|s| s.proxy_override)
        .unwrap_or_default();
    ProxySnapshot::from_global(global_config, global_system).with_override(plugin_override)
}

/// Safe human-readable summary of a global [`ProxyConfig`] (NEVER contains passwords).
pub fn describe_global_proxy(config: &ProxyConfig, system: &SystemProxySnapshot) -> String {
    match config.mode {
        ProxyMode::Direct => "全局：直连（不使用代理）".into(),
        ProxyMode::Custom => match &config.custom {
            Some(c) => format!("全局：自定义 {}", c.to_endpoint().redacted_url()),
            None => "全局：自定义（未配置）".into(),
        },
        ProxyMode::System => {
            if let Some(ep) = system
                .https
                .as_ref()
                .or(system.http.as_ref())
                .or(system.socks5.as_ref())
            {
                format!("全局：跟随系统（{}）", ep.redacted_url())
            } else {
                "全局：跟随系统（直连）".into()
            }
        }
    }
}

fn ensure_private_directory(path: &Path) -> AppResult<()> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(|_| AppError::state())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|_| AppError::state())?;
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| AppError::state())?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::state());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.mode() & 0o777 != 0o700 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(AppError::state());
        }
    }
    Ok(())
}

fn check_private_file(metadata: &fs::Metadata) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.mode() & 0o777 != 0o600
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err(AppError::state());
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_file_defaults_to_inherit_and_roundtrips_securely() {
        let root = tempfile::tempdir().unwrap();
        let store = PluginProxyStore::at_root(root.path().join("plugin-data")).unwrap();
        assert!(!store.path().exists());
        assert_eq!(
            store.load().unwrap().proxy_override,
            ProxyOverride::Inherit
        );
        assert!(!store.path().exists(), "load must not create file");

        let custom = CustomProxyConfig {
            protocol: ProxyProtocol::Socks5h,
            host: "127.0.0.1".into(),
            port: 7890,
            username: Some("alice".into()),
            password: Some("plugin-secret-pw".into()),
            bypass: vec!["*.internal".into()],
        };
        let settings = PluginProxySettings::from_override(ProxyOverride::Custom(custom.clone()));
        assert!(!format!("{settings:?}").contains("plugin-secret-pw"));
        store.save(&settings).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let loaded = store.load().unwrap();
        assert_eq!(loaded.proxy_override, ProxyOverride::Custom(custom.clone()));
        assert_eq!(loaded.saved_custom, Some(custom));

        // Corrupt / future file fails closed and is never overwritten.
        fs::write(
            store.path(),
            r#"{"format_version":99,"override":{"mode":"direct"}}"#,
        )
        .unwrap();
        assert!(store.load().is_err());
        assert!(store.save(&PluginProxySettings::inherit()).is_err());
        assert!(fs::read_to_string(store.path())
            .unwrap()
            .contains("\"format_version\":99"));
    }
}
