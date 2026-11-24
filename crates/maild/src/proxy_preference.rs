//! proxy_preference：全局代理偏好与系统代理热更新服务。
//!
//! 这是 daemon 拥有的**持久偏好**，与 `reading_policy.toml` / `locale_preference.toml` 同级：
//! `proxy_preference.toml` 与活动配置同目录，复用 `crate::plugin_state::{check_owned_file,
//! publish_atomically}`（Unix `0600`、当前用户拥有、单硬链接、拒符号链接、64 KiB 上限、
//! `format_version = 1`、`#[serde(deny_unknown_fields)]`、损坏/未来版本 fail-closed 且绝不覆盖、
//! 原子写发布）。不进 SQLite，天然排除在 SQLite 备份之外。
//!
//! 凭据（可选 `username` / `password`）直接存于本 `0600` 文件，不经 OS Keychain，不建本地回环
//! 中继。密码永不打印到日志、`Debug` 或错误消息中。

use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(any(test, debug_assertions))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cordis::{Context, Service};
use maild_proxy::{
    detect_system_proxy, validate_custom_config, validate_proxy_config, CustomProxyConfig,
    ProxyConfig, ProxyConnector, ProxyMode, ProxyProtocol, SystemProxySnapshot,
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::events::{ProxyPreferenceChanged, ProxyPreferenceEvent};

/// 代理偏好文件名（活动配置同目录）。
pub const PREFERENCE_FILE: &str = "proxy_preference.toml";
const FORMAT_VERSION: i64 = 1;
const MAX_PREFERENCE_BYTES: u64 = 64 * 1024;

/// 磁盘形态。未知字段拒绝（fail-closed）。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProxyPreferenceFile {
    format_version: i64,
    mode: ProxyMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    custom: Option<CustomProxyConfig>,
}

/// `setProxyConfig` / `testProxy` 中 `custom` 字段的更新载荷。
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomProxyUpdate {
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    /// 省略（`None`）时保留现有已存密码（除非 `clear_password = true`）。
    #[serde(default)]
    pub password: Option<String>,
    /// 显式清空已保存密码。与 `password` 互斥。
    #[serde(default)]
    pub clear_password: bool,
    #[serde(default)]
    pub bypass: Vec<String>,
}

impl fmt::Debug for CustomProxyUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomProxyUpdate")
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("clear_password", &self.clear_password)
            .field("bypass", &self.bypass)
            .finish()
    }
}

/// `setProxyConfig` 参数。未知字段拒绝。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfigUpdate {
    pub mode: ProxyMode,
    #[serde(default)]
    pub custom: Option<CustomProxyUpdate>,
    /// 允许顶层清空已保存密码（例如在保留 `custom` 其余字段时仅清除密码）。
    #[serde(default)]
    pub clear_password: bool,
}

/// `getProxyConfig` / `setProxyConfig` 的应答形状。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProxyConfigView {
    pub config: ProxyConfig,
    pub has_password: bool,
    pub effective_system: SystemProxySnapshot,
    pub persisted: bool,
}

/// 写入失败的三种原因，API 层分别映射 `-32602` / `-32001` / `-32603`。
#[derive(Debug, PartialEq, Eq)]
pub enum SetError {
    Invalid(String),
    Unavailable(String),
    Io(String),
}

enum State {
    Ready(ProxyConfig),
    /// 文件损坏/未来版本：读返回缺省、写一律拒绝且**绝不覆盖**磁盘上的那份。
    Unavailable(String),
}

type SystemProxyReader = Arc<dyn Fn() -> SystemProxySnapshot + Send + Sync>;

/// 全局代理偏好与连接器工厂服务。
pub struct ProxyService {
    path: PathBuf,
    state: Mutex<State>,
    generation: watch::Sender<u64>,
    system_reader: SystemProxyReader,
    last_system: Mutex<SystemProxySnapshot>,
    #[cfg(target_os = "macos")]
    watch_os_changes: bool,
    #[cfg(target_os = "macos")]
    watcher: Mutex<Option<maild_proxy::SystemProxyWatcher>>,
    #[cfg(any(test, debug_assertions))]
    allow_loopback_proxy: AtomicBool,
    #[cfg(any(test, debug_assertions))]
    accept_invalid_proxy_certs: AtomicBool,
}

impl Service for ProxyService {
    fn name(&self) -> &'static str {
        "proxy.preference"
    }
}

impl ProxyService {
    /// 从磁盘加载；文件不存在 → 缺省偏好（`system`）Ready（只读不创建文件）。
    pub fn open(path: PathBuf) -> Self {
        Self::open_inner(path, Arc::new(detect_system_proxy), true)
    }

    /// 测试用入口：注入固定/可变的系统代理读取器，不挂真实 OS watcher。
    pub fn open_with_system(
        path: PathBuf,
        system: impl Fn() -> SystemProxySnapshot + Send + Sync + 'static,
    ) -> Self {
        Self::open_inner(path, Arc::new(system), false)
    }

    fn open_inner(path: PathBuf, system_reader: SystemProxyReader, watch_os_changes: bool) -> Self {
        let state = match load_file(&path) {
            Ok(config) => State::Ready(config),
            Err(reason) => {
                eprintln!(
                    "[maild/proxy] {} 无法加载，代理偏好停摆（读返回缺省、写拒绝、文件不覆盖）: {reason}",
                    path.display()
                );
                State::Unavailable(reason)
            }
        };
        let initial_system = (system_reader)();
        let (generation, _) = watch::channel(0u64);
        #[cfg(not(target_os = "macos"))]
        let _ = watch_os_changes;
        Self {
            path,
            state: Mutex::new(state),
            generation,
            system_reader,
            last_system: Mutex::new(initial_system),
            #[cfg(target_os = "macos")]
            watch_os_changes,
            #[cfg(target_os = "macos")]
            watcher: Mutex::new(None),
            #[cfg(any(test, debug_assertions))]
            allow_loopback_proxy: AtomicBool::new(false),
            #[cfg(any(test, debug_assertions))]
            accept_invalid_proxy_certs: AtomicBool::new(false),
        }
    }

    /// 在 macOS 上启动 `SCDynamicStore` `State:/Network/Global/Proxies` 推送监听（零轮询，500ms 防抖）。
    ///
    /// 系统代理快照变化时自增 `generation` 并向事件总线广播 [`ProxyPreferenceChanged`]。
    pub fn start_system_watcher(self: &Arc<Self>, ctx: &Context) {
        #[cfg(target_os = "macos")]
        {
            if !self.watch_os_changes {
                return;
            }
            let initial = self.last_system.lock().unwrap().clone();
            let weak = Arc::downgrade(self);
            let emit_ctx = ctx.clone();
            let watcher = maild_proxy::SystemProxyWatcher::start(
                initial,
                Arc::clone(&self.system_reader),
                Arc::new(move |next_system| {
                    let Some(service) = weak.upgrade() else {
                        return;
                    };
                    let changed = {
                        let mut guard = service.last_system.lock().unwrap();
                        if *guard == next_system {
                            false
                        } else {
                            *guard = next_system.clone();
                            true
                        }
                    };
                    if changed {
                        let next_gen = service.bump_generation();
                        let event = service.build_event(&next_system, next_gen);
                        emit_ctx.emit::<ProxyPreferenceChanged>(event);
                    }
                }),
            );
            *self.watcher.lock().unwrap() = watcher;
            let teardown = Arc::downgrade(self);
            let _ = ctx.try_effect_sync(move || {
                if let Some(service) = teardown.upgrade() {
                    service.stop_system_watcher();
                }
            });
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = ctx;
        }
    }

    pub fn stop_system_watcher(&self) {
        #[cfg(target_os = "macos")]
        {
            self.watcher.lock().unwrap().take();
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_ready(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), State::Ready(_))
    }

    pub fn generation(&self) -> u64 {
        *self.generation.borrow()
    }

    pub fn subscribe_generation(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }

    fn bump_generation(&self) -> u64 {
        let mut next = 0;
        self.generation.send_modify(|value| {
            *value = value.wrapping_add(1);
            next = *value;
        });
        next
    }

    /// 当前配置快照（含内存中的密码，仅进程内使用）。停摆时返回缺省 `System` 模式。
    pub fn snapshot(&self) -> ProxyConfig {
        match &*self.state.lock().unwrap() {
            State::Ready(config) => config.clone(),
            State::Unavailable(_) => ProxyConfig::default(),
        }
    }

    /// 读取当前系统代理快照，并同步更新内部缓存的最后快照。
    pub fn effective_system(&self) -> SystemProxySnapshot {
        let next = (self.system_reader)();
        *self.last_system.lock().unwrap() = next.clone();
        next
    }

    /// 供测试或手动刷新调用：重读系统代理，若与上次不同则递增 generation 并广播事件。
    pub fn refresh_system_proxy(&self, ctx: Option<&Context>) -> bool {
        let next = (self.system_reader)();
        let changed = {
            let mut guard = self.last_system.lock().unwrap();
            if *guard == next {
                false
            } else {
                *guard = next.clone();
                true
            }
        };
        if changed {
            let gen = self.bump_generation();
            if let Some(ctx) = ctx {
                let event = self.build_event(&next, gen);
                ctx.emit::<ProxyPreferenceChanged>(event);
            }
        }
        changed
    }

    /// 对外视图。`include_secret = false` 时剥离自定义代理密码与系统环境变量代理密码。
    pub fn view(&self, include_secret: bool) -> ProxyConfigView {
        let (config, persisted) = match &*self.state.lock().unwrap() {
            State::Ready(cfg) => (cfg.clone(), true),
            State::Unavailable(_) => (ProxyConfig::default(), false),
        };
        let has_password = config.has_password();
        let system = self.effective_system();
        ProxyConfigView {
            config: if include_secret {
                config
            } else {
                config.without_secret()
            },
            has_password,
            effective_system: if include_secret {
                system
            } else {
                system.without_secrets()
            },
            persisted,
        }
    }

    fn build_event(&self, system: &SystemProxySnapshot, generation: u64) -> ProxyPreferenceEvent {
        let (config, persisted) = match &*self.state.lock().unwrap() {
            State::Ready(cfg) => (cfg.clone(), true),
            State::Unavailable(_) => (ProxyConfig::default(), false),
        };
        ProxyPreferenceEvent {
            has_password: config.has_password(),
            config: config.without_secret(),
            effective_system: system.without_secrets(),
            persisted,
            generation,
        }
    }

    /// 更新并原子持久化代理配置。返回 `(非秘密视图, 变更事件, 是否发生实质变化)`。
    pub fn set(
        &self,
        update: &ProxyConfigUpdate,
    ) -> Result<(ProxyConfigView, ProxyPreferenceEvent, bool), SetError> {
        let (next, changed) = {
            let mut state = self.state.lock().unwrap();
            let current = match &*state {
                State::Ready(cfg) => cfg,
                State::Unavailable(reason) => {
                    return Err(SetError::Unavailable(reason.clone()));
                }
            };
            let next = resolve_update(current, update, true).map_err(SetError::Invalid)?;
            let changed = next != *current;
            save_file(&self.path, &next).map_err(SetError::Io)?;
            *state = State::Ready(next.clone());
            (next, changed)
        };

        let system = self.effective_system();
        let gen = if changed {
            self.bump_generation()
        } else {
            self.generation()
        };
        let has_password = next.has_password();
        let view = ProxyConfigView {
            config: next.without_secret(),
            has_password,
            effective_system: system.without_secrets(),
            persisted: true,
        };
        let event = ProxyPreferenceEvent {
            config: view.config.clone(),
            has_password,
            effective_system: view.effective_system.clone(),
            persisted: true,
            generation: gen,
        };
        Ok((view, event, changed))
    }

    /// 解析 `testProxy` 的未保存草稿。`use_saved_password = true` 时在未提供新密码的情况下复用已保存密码。
    pub fn resolve_draft(
        &self,
        update: &ProxyConfigUpdate,
        use_saved_password: bool,
    ) -> Result<ProxyConfig, String> {
        let current = self.snapshot();
        resolve_update(&current, update, use_saved_password)
    }

    /// 构造使用当前活动配置与系统快照的 [`ProxyConnector`]。
    pub fn connector(&self) -> ProxyConnector {
        self.connector_for(self.snapshot())
    }

    /// 构造使用指定配置与当前系统快照的 [`ProxyConnector`]（供 `testProxy` 草稿测试）。
    pub fn connector_for(&self, config: ProxyConfig) -> ProxyConnector {
        let system = self.effective_system();
        let connector = ProxyConnector::new(config, system);
        #[cfg(any(test, debug_assertions))]
        let connector = connector
            .with_allow_loopback_proxy(self.allow_loopback_proxy.load(Ordering::Relaxed))
            .with_accept_invalid_proxy_certs(
                self.accept_invalid_proxy_certs.load(Ordering::Relaxed)
                    || crate::credential::test_accept_invalid_certs(),
            );
        connector
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_allow_loopback_proxy(&self, allow: bool) {
        self.allow_loopback_proxy.store(allow, Ordering::Relaxed);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_accept_invalid_proxy_certs(&self, accept: bool) {
        self.accept_invalid_proxy_certs
            .store(accept, Ordering::Relaxed);
    }
}

/// 合并更新载荷与当前配置并校验。
///
/// - `inherit_saved_password = true`：`custom.password` 省略时保留现有密码（`setProxyConfig` 语义，或 `testProxy` 带 `use_saved_password: true`）。
/// - `inherit_saved_password = false`：`custom.password` 省略时视为无密码（`testProxy` 测试全新草稿）。
fn resolve_update(
    current: &ProxyConfig,
    update: &ProxyConfigUpdate,
    inherit_saved_password: bool,
) -> Result<ProxyConfig, String> {
    let custom = match &update.custom {
        Some(upd) => {
            let clear = update.clear_password || upd.clear_password;
            if clear && upd.password.is_some() {
                return Err("`password` 与 `clear_password = true` 不能同时给出".into());
            }
            let password = if clear {
                None
            } else if let Some(pw) = &upd.password {
                if pw.is_empty() {
                    None
                } else {
                    Some(pw.clone())
                }
            } else if inherit_saved_password {
                current.custom.as_ref().and_then(|c| c.password.clone())
            } else {
                None
            };
            let candidate = CustomProxyConfig {
                protocol: upd.protocol,
                host: upd.host.clone(),
                port: upd.port,
                username: upd.username.clone(),
                password,
                bypass: upd.bypass.clone(),
            };
            Some(validate_custom_config(&candidate)?)
        }
        None => {
            // 切换到 system/direct（或保留已有 custom）时，自动保留先前的 custom 配置。
            let mut preserved = current.custom.clone();
            if update.clear_password || !inherit_saved_password {
                if let Some(cfg) = preserved.as_mut() {
                    cfg.password = None;
                }
            }
            preserved
        }
    };

    validate_proxy_config(&ProxyConfig {
        mode: update.mode,
        custom,
    })
}

fn load_file(path: &Path) -> Result<ProxyConfig, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProxyConfig::default())
        }
        Err(error) => return Err(format!("不可检查: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("类型无效（符号链接或非普通文件）".into());
    }
    crate::plugin_state::check_owned_file(&metadata)?;
    let bytes = std::fs::read(path).map_err(|error| format!("不可读取: {error}"))?;
    if bytes.len() as u64 > MAX_PREFERENCE_BYTES {
        return Err(format!("超过 {MAX_PREFERENCE_BYTES} 字节上限"));
    }
    let text = String::from_utf8(bytes).map_err(|_| "不是有效的 UTF-8".to_string())?;
    let table: toml::Table =
        toml::from_str(&text).map_err(|error| format!("解析失败: {error}"))?;
    match table
        .get("format_version")
        .and_then(toml::Value::as_integer)
    {
        Some(FORMAT_VERSION) => {}
        Some(other) => {
            return Err(format!(
                "未来 format_version {other}（当前支持 {FORMAT_VERSION}）"
            ))
        }
        None => return Err("缺少 format_version".into()),
    }
    let file: ProxyPreferenceFile = table
        .try_into()
        .map_err(|error| format!("解析失败: {error}"))?;
    validate_proxy_config(&ProxyConfig {
        mode: file.mode,
        custom: file.custom,
    })
}

fn save_file(path: &Path, config: &ProxyConfig) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("代理偏好文件路径缺少父目录: {path:?}"))?;
    let file = ProxyPreferenceFile {
        format_version: FORMAT_VERSION,
        mode: config.mode,
        custom: config.custom.clone(),
    };
    let body = toml::to_string_pretty(&file).map_err(|error| format!("序列化失败: {error}"))?;
    if body.len() as u64 > MAX_PREFERENCE_BYTES {
        return Err(format!("配置体积超过 {MAX_PREFERENCE_BYTES} 字节上限"));
    }
    let temp = parent.join(format!(
        ".{PREFERENCE_FILE}.tmp-{}",
        crate::plugin_identity::generate_token()
    ));
    let result = crate::plugin_state::publish_atomically(&temp, path, body.as_bytes());
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// 代理偏好文件路径（活动配置同目录）。
pub fn preference_path_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(PREFERENCE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secure_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        dir
    }

    fn write_pref(dir: &Path, body: &str) {
        std::fs::write(dir.join(PREFERENCE_FILE), body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                dir.join(PREFERENCE_FILE),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
    }

    fn open(path: PathBuf) -> ProxyService {
        ProxyService::open_with_system(path, SystemProxySnapshot::default)
    }

    #[test]
    fn missing_file_defaults_to_system_and_roundtrips_with_password_preservation() {
        let dir = secure_dir();
        let path = dir.path().join(PREFERENCE_FILE);
        let svc = open(path.clone());
        assert!(svc.is_ready());
        assert_eq!(svc.snapshot().mode, ProxyMode::System);
        assert!(!path.exists(), "只读不落盘");

        let (view, event, changed) = svc
            .set(&ProxyConfigUpdate {
                mode: ProxyMode::Custom,
                custom: Some(CustomProxyUpdate {
                    protocol: ProxyProtocol::Socks5h,
                    host: "127.0.0.1".into(),
                    port: 7890,
                    username: Some("alice".into()),
                    password: Some("secret-1".into()),
                    clear_password: false,
                    bypass: vec!["*.local".into()],
                }),
                clear_password: false,
            })
            .unwrap();
        assert!(changed);
        assert!(view.has_password);
        assert_eq!(view.config.custom.as_ref().unwrap().password, None);
        assert_eq!(event.generation, 1);
        assert_eq!(svc.view(true).config.custom.as_ref().unwrap().password.as_deref(), Some("secret-1"));
        assert_eq!(svc.view(false).config.custom.as_ref().unwrap().password, None);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        // Switching to Direct preserves custom config + password!
        let (view, event, changed) = svc
            .set(&ProxyConfigUpdate {
                mode: ProxyMode::Direct,
                custom: None,
                clear_password: false,
            })
            .unwrap();
        assert!(changed);
        assert_eq!(event.generation, 2);
        assert_eq!(view.config.mode, ProxyMode::Direct);
        assert!(view.has_password);
        assert_eq!(view.config.custom.as_ref().unwrap().port, 7890);

        // Updating custom without `password` preserves existing password.
        let (view, _, _) = svc
            .set(&ProxyConfigUpdate {
                mode: ProxyMode::Custom,
                custom: Some(CustomProxyUpdate {
                    protocol: ProxyProtocol::Http,
                    host: "127.0.0.1".into(),
                    port: 8080,
                    username: Some("alice".into()),
                    password: None,
                    clear_password: false,
                    bypass: vec![],
                }),
                clear_password: false,
            })
            .unwrap();
        assert!(view.has_password);
        assert_eq!(
            svc.snapshot().custom.as_ref().unwrap().password.as_deref(),
            Some("secret-1")
        );

        // Clearing password explicitly works.
        let (view, _, _) = svc
            .set(&ProxyConfigUpdate {
                mode: ProxyMode::Custom,
                custom: None,
                clear_password: true,
            })
            .unwrap();
        assert!(!view.has_password);
        assert_eq!(svc.snapshot().custom.as_ref().unwrap().password, None);

        // Reopen from disk preserves state.
        let reopened = open(path);
        assert!(reopened.is_ready());
        assert_eq!(reopened.snapshot().mode, ProxyMode::Custom);
        assert_eq!(reopened.snapshot().custom.as_ref().unwrap().port, 8080);
    }

    #[test]
    fn corrupt_or_future_files_fail_closed_without_overwriting() {
        for body in [
            "format_version = 99\nmode = \"system\"\n",
            "mode = \"system\"\n",
            "format_version = 1\nmode = \"system\"\nunknown_field = 1\n",
            "format_version = 1\nmode = \"custom\"\n",
            "format_version = 1\nmode = \"custom\"\n[custom]\nprotocol = \"http\"\nhost = \"\"\nport = 8080\n",
            "not valid toml = = =",
        ] {
            let dir = secure_dir();
            write_pref(dir.path(), body);
            let path = dir.path().join(PREFERENCE_FILE);
            let svc = open(path.clone());
            assert!(!svc.is_ready(), "{body:?} must stall");
            assert_eq!(svc.snapshot(), ProxyConfig::default());
            assert!(!svc.view(false).persisted);
            assert!(matches!(
                svc.set(&ProxyConfigUpdate {
                    mode: ProxyMode::Direct,
                    custom: None,
                    clear_password: false,
                }),
                Err(SetError::Unavailable(_))
            ));
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                body,
                "corrupt file must never be overwritten"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn wide_permissions_and_symlinks_are_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = secure_dir();
        write_pref(dir.path(), "format_version = 1\nmode = \"direct\"\n");
        std::fs::set_permissions(
            dir.path().join(PREFERENCE_FILE),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(!open(dir.path().join(PREFERENCE_FILE)).is_ready());

        let dir = secure_dir();
        let target = dir.path().join("other.toml");
        std::fs::write(&target, "format_version = 1\nmode = \"direct\"\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(PREFERENCE_FILE)).unwrap();
        assert!(!open(dir.path().join(PREFERENCE_FILE)).is_ready());
    }
}
