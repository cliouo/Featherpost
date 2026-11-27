//! Reusable daemon runner shared by the interactive `maild` CLI and platform agents.
//!
//! Callers resolve the configuration source, then pass an absolute path here. The runner owns
//! signal installation, configuration validation, plugin assembly, readiness waiting and LIFO
//! teardown so every host gets the same lifecycle semantics.

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};

use cordis::{Context, FiberState};
use maild_paths::AppPaths;

use crate::api::{config_from_toml, validate_api_config, validate_config_file_security};
use crate::loader::plugin_config;
use crate::paths::{assemble_with_paths_from, resolve_api_path, resolve_store_path};
use crate::store::StoreConfig;
use crate::Accounts;

// —— 常驻进程的内存与调度姿态（ADR-0041 决策 6）——

#[cfg(target_os = "macos")]
extern "C" {
    /// libmalloc：让分配器把空闲页还给内核。`zone = NULL` 表示所有 zone，`goal = 0` 表示尽量多。
    fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: libc::size_t) -> libc::size_t;
}

/// 把分配器手里的空闲堆页立即归还给内核（macOS），降低 Activity Monitor 里的「内存」
/// （`phys_footprint`）。其他平台为 no-op。
///
/// 在一段内存高峰之后调用：账户首次上线、检索索引回填完成、冷启动批量同步完成。
/// 开销是一次遍历各 zone 的空闲表，不要放在热路径上。
pub fn release_unused_memory() {
    #[cfg(target_os = "macos")]
    // SAFETY: libmalloc 公开 API；NULL zone 是文档化的「全部 zone」，无其他前置条件。
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

/// 当前进程的常驻内存 `(rss_bytes, phys_footprint_bytes)`。
///
/// - macOS：`proc_pid_rusage(RUSAGE_INFO_V4)` 的 `ri_resident_size` 与 `ri_phys_footprint`
///   （后者就是 Activity Monitor「内存」列的口径）；
/// - Linux：`/proc/self/statm` 的常驻页数，没有 footprint 口径；
/// - 其他平台：`None`。
pub fn current_process_memory_bytes() -> Option<(u64, Option<u64>)> {
    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: 缓冲是一整个 rusage_info_v4，flavor 与之匹配；内核只写不读。
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr() as *mut libc::rusage_info_t,
            )
        };
        if rc != 0 {
            return None;
        }
        // SAFETY: rc == 0 表示内核已填充整个结构。
        let info = unsafe { info.assume_init() };
        Some((info.ri_resident_size, Some(info.ri_phys_footprint)))
    }
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: sysconf 无前置条件。
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 {
            return None;
        }
        Some((resident_pages.saturating_mul(page_size as u64), None))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// 把当前线程（及其之后创建的线程）标成 macOS `UTILITY` QoS：常驻收信是后台工作，
/// 让出 CPU / 定时器合并给前台应用，但不像 `BACKGROUND` 那样把 API 应答也节流掉。
/// 其他平台为 no-op。只应由平台托管的后台 Agent 在启动运行时之前调用。
pub fn apply_background_qos() {
    #[cfg(target_os = "macos")]
    // SAFETY: 只修改调用线程自身的 QoS；失败返回非零，忽略即可。
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

/// Fully resolved inputs for one daemon run.
#[derive(Clone, Debug)]
pub struct DaemonOptions {
    config_path: PathBuf,
    app_paths: AppPaths,
    legacy_cwd: Option<PathBuf>,
    recover_managed_socket: bool,
    managed_agent_control: bool,
}

impl DaemonOptions {
    /// Build production runner options. All managed roots must be absolute so the runner never
    /// acquires an accidental dependency on its process working directory.
    pub fn new(config_path: PathBuf, app_paths: AppPaths) -> Result<Self, DaemonError> {
        for (name, path) in [
            ("配置", config_path.as_path()),
            ("data", app_paths.data_dir.as_path()),
            ("runtime", app_paths.runtime_dir.as_path()),
        ] {
            if !path.is_absolute() {
                return Err(DaemonError::configuration(format!(
                    "daemon {name} 路径必须是绝对路径: {path:?}"
                )));
            }
        }
        Ok(Self {
            config_path,
            app_paths,
            legacy_cwd: None,
            recover_managed_socket: false,
            managed_agent_control: false,
        })
    }

    /// Enable the interactive CLI's legacy-CWD collision guard. Platform agents leave this unset.
    pub fn with_legacy_cwd(mut self, cwd: Option<PathBuf>) -> Self {
        self.legacy_cwd = cwd;
        self
    }

    /// Let a platform-managed Agent reclaim its own stale Unix socket after the OS terminates a
    /// Login Item without running the daemon teardown path. Recovery remains limited to the
    /// current user's validated managed runtime directory; interactive daemons stay fail-closed.
    pub fn with_managed_socket_recovery(mut self) -> Self {
        self.recover_managed_socket = true;
        self
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Only the platform Agent host opts into the user-only graceful-stop service.
    pub fn with_managed_agent_control(mut self) -> Self {
        self.managed_agent_control = true;
        self
    }
}

#[derive(Debug)]
pub struct DaemonError {
    kind: DaemonErrorKind,
    message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonErrorKind {
    ConfigurationStop,
    Unexpected,
}

impl DaemonError {
    fn configuration(message: impl Into<String>) -> Self {
        Self {
            kind: DaemonErrorKind::ConfigurationStop,
            message: message.into(),
        }
    }

    fn unexpected(message: impl Into<String>) -> Self {
        Self {
            kind: DaemonErrorKind::Unexpected,
            message: message.into(),
        }
    }

    /// Whether a platform host should stop cleanly instead of asking its supervisor to restart.
    pub fn is_configuration_stop(&self) -> bool {
        self.kind == DaemonErrorKind::ConfigurationStop
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DaemonError {}

/// Run the daemon until an OS stop signal arrives.
///
/// Signal receivers are installed synchronously before the configuration is read or any plugin
/// resource is assembled. SIGINT/SIGTERM on Unix and the service-console stop signals on Windows
/// therefore share the same LIFO teardown path.
pub async fn run_daemon(options: DaemonOptions) -> Result<(), DaemonError> {
    let mut signals = ShutdownSignals::install().map_err(|error| {
        DaemonError::unexpected(format!("安装停止信号处理器失败: {}", terminal_text(&error)))
    })?;
    run_daemon_until(options, async move { signals.recv().await }).await
}

/// 与 [`run_daemon`] 相同，但由调用方提供停止 future（测试与宿主复用）。
pub async fn run_daemon_until<F>(options: DaemonOptions, shutdown: F) -> Result<(), DaemonError>
where
    F: Future<Output = ()>,
{
    let path = options.config_path();
    let toml_text = std::fs::read_to_string(path).map_err(|error| {
        DaemonError::configuration(format!("读配置失败 {path:?}: {}", terminal_text(&error)))
    })?;
    let mut api_config = config_from_toml(&toml_text).map_err(|error| {
        DaemonError::configuration(format!("配置错误: {}", terminal_text(&error)))
    })?;
    let mut store_config = plugin_config::<StoreConfig>(&toml_text, "store").map_err(|error| {
        DaemonError::configuration(format!("配置错误: {}", terminal_text(&error)))
    })?;
    let frontend_config = plugin_config::<crate::frontend::FrontendConfig>(&toml_text, "frontend")
        .map_err(|error| {
            DaemonError::configuration(format!("配置错误: {}", terminal_text(&error)))
        })?;
    let external_config = plugin_config::<crate::external::ExternalConfig>(&toml_text, "external")
        .map_err(|error| {
            DaemonError::configuration(format!("配置错误: {}", terminal_text(&error)))
        })?;

    let resolved_api_path = api_config
        .as_ref()
        .map(|config| resolve_api_path(config, &options.app_paths))
        .transpose()
        .map_err(|error| {
            DaemonError::configuration(format!("配置错误: {}", terminal_text(&error)))
        })?
        .flatten();
    let api_path_needs_preparation = resolved_api_path.is_some()
        && api_config
            .as_ref()
            .is_some_and(|config| !Path::new(&config.listen).is_absolute());
    if let (Some(config), Some(resolved)) = (&mut api_config, resolved_api_path) {
        config.listen = resolved.to_string_lossy().into_owned();
    }

    #[cfg(unix)]
    if options.recover_managed_socket {
        if let Some(config) = &api_config {
            recover_managed_unix_socket(config, &options.app_paths).map_err(|error| {
                DaemonError::configuration(format!(
                    "配置错误: 无法恢复受管 API socket: {}",
                    terminal_text(&error)
                ))
            })?;
        }
    }

    let resolved_store_path = store_config
        .as_ref()
        .map(|config| resolve_store_path(config, &options.app_paths))
        .transpose()
        .map_err(|error| {
            DaemonError::configuration(format!("配置错误: {}", terminal_text(&error)))
        })?
        .flatten();
    if let (Some(config), Some(resolved)) = (&mut store_config, resolved_store_path) {
        config.db_path = resolved.to_string_lossy().into_owned();
    }

    if let Some(config) = &api_config {
        if config.transport == "http" {
            validate_config_file_security(path, config).map_err(|error| {
                DaemonError::configuration(format!("配置安全错误: {}", terminal_text(&error)))
            })?;
        }
        if !api_path_needs_preparation {
            validate_api_config(config).map_err(|error| {
                DaemonError::configuration(format!("配置错误: {}", terminal_text(&error)))
            })?;
        }
    }

    let ctx = Context::new();
    let managed_control = options.managed_agent_control.then(|| {
        let control = std::sync::Arc::new(crate::agent_control::ManagedAgentControl::default());
        ctx.provide(control.clone());
        control
    });
    // M4-05（ADR-0021）：provider 注册表与具体插件装配解耦——external 未启用时
    // 委托调用按 -32018 快速失败，而不是服务缺失的内部错误。
    ctx.provide(crate::provider::ProviderRegistry::new());
    // ADR-0050：外部翻译 provider 注册表同样经 Cordis ctx.provide 挂载，与引擎完全解耦。
    ctx.provide(crate::translation_provider::TranslationProviderRegistry::new());
    let mut assembly = assemble_with_paths_from(
        &ctx,
        &toml_text,
        &options.app_paths,
        options.legacy_cwd.as_deref(),
        path,
    )
    .map_err(|error| DaemonError::configuration(format!("配置错误: {}", terminal_text(&error))))?;

    if store_config.is_some() && !plugin_is_active(&assembly, "store") {
        let mut message = String::from("配置错误: store 插件未能取得数据库所有权或完成装配");
        for error in &assembly.errors {
            if matches!(error, crate::loader::LoadError::Apply { plugin, .. } if plugin == "store")
            {
                message.push_str("\n  ");
                message.push_str(&terminal_text(error));
            }
        }
        assembly.dispose().await;
        return Err(DaemonError::configuration(message));
    }

    if api_config
        .as_ref()
        .is_some_and(|config| config.transport != "stdio")
        && !plugin_is_active(&assembly, "api")
    {
        let transport = api_config
            .as_ref()
            .map(|config| match config.transport.as_str() {
                "http" => "HTTP",
                "unix" | "unix_socket" | "socket" => "Unix socket",
                "named_pipe" | "pipe" => "named pipe",
                "ipc" => "IPC",
                _ => "listener",
            })
            .unwrap_or("listener");
        assembly.dispose().await;
        return Err(DaemonError::configuration(format!(
            "配置错误: api {transport} 插件未能启动"
        )));
    }

    if frontend_config.is_some() && !plugin_is_active(&assembly, "frontend") {
        let mut message = String::from("配置错误: frontend 插件未能完成受检目录装配");
        for error in &assembly.errors {
            if matches!(error, crate::loader::LoadError::Apply { plugin, .. } if plugin == "frontend")
            {
                message.push_str("\n  ");
                message.push_str(&terminal_text(error));
            }
        }
        assembly.dispose().await;
        return Err(DaemonError::configuration(message));
    }

    if external_config.is_some() && !plugin_is_active(&assembly, "external") {
        let mut message = String::from("配置错误: external 插件未能完成受检目录装配");
        for error in &assembly.errors {
            if matches!(error, crate::loader::LoadError::Apply { plugin, .. } if plugin == "external")
            {
                message.push_str("\n  ");
                message.push_str(&terminal_text(error));
            }
        }
        assembly.dispose().await;
        return Err(DaemonError::configuration(message));
    }

    for error in &assembly.errors {
        eprintln!("[maild] 装配警告: {}", terminal_text(error));
    }
    // stdout is reserved for the stdio JSON-RPC transport.
    eprintln!("[maild] 装配报告:");
    for (name, state) in assembly.report() {
        eprintln!("  {:<12} {state:?}", terminal_text(&name));
    }

    let mut shutdown = Box::pin(async move {
        if let Some(control) = managed_control {
            tokio::select! { _ = shutdown => {}, _ = control.wait() => {} }
        } else { shutdown.await; }
    });
    let mut shutting_down = false;
    if let Some(accounts) = ctx.get::<Accounts>() {
        for (name, _, receive) in accounts.snapshot() {
            tokio::select! {
                _ = &mut shutdown => {
                    shutting_down = true;
                    break;
                }
                result = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    receive.wait_online(),
                ) => {
                    match result {
                        Ok(()) => eprintln!("[maild] 账户 {} 在线", terminal_text(&name)),
                        Err(_) => eprintln!(
                            "[maild] 账户 {} 30s 内未连上，检查服务器配置",
                            terminal_text(&name)
                        ),
                    }
                }
            }
        }
    }

    if !shutting_down {
        // 装配与首次同步的内存高峰已过：把空闲堆页还给内核，常驻期从低水位开始。
        release_unused_memory();
        eprintln!("[maild] 常驻运行中，Ctrl+C 或服务停止信号退出");
        shutdown.await;
    }
    eprintln!("[maild] 关闭中（LIFO 卸载插件）…");
    assembly.dispose().await;
    eprintln!("[maild] 已退出");
    Ok(())
}

fn plugin_is_active(assembly: &crate::loader::Assembly, plugin: &str) -> bool {
    assembly
        .report()
        .iter()
        .any(|(name, state)| name == plugin && *state == FiberState::Active)
}

#[cfg(unix)]
fn recover_managed_unix_socket(
    config: &crate::api::ApiConfig,
    app_paths: &AppPaths,
) -> Result<(), String> {
    use std::io;
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    if !matches!(
        config.transport.as_str(),
        "unix" | "unix_socket" | "socket" | "ipc"
    ) {
        return Ok(());
    }

    let path = Path::new(&config.listen);
    if !path.starts_with(&app_paths.runtime_dir) {
        return Ok(());
    }

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取 {path:?} 失败: {error}")),
    };

    let managed_root = std::fs::canonicalize(&app_paths.runtime_dir)
        .map_err(|error| format!("规范化受管 runtime 目录失败: {error}"))?;
    let socket_parent = path
        .parent()
        .ok_or_else(|| format!("受管 socket 路径缺少父目录: {path:?}"))?;
    let canonical_parent = std::fs::canonicalize(socket_parent)
        .map_err(|error| format!("规范化受管 socket 父目录失败: {error}"))?;
    if !canonical_parent.starts_with(&managed_root) {
        return Err(format!(
            "{path:?} 解析后逃出受管 runtime 目录，拒绝自动处理"
        ));
    }
    let current_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_socket() || metadata.uid() != current_uid {
        return Err(format!(
            "{path:?} 不是当前用户拥有的 Unix socket，拒绝自动处理"
        ));
    }

    // This repeats the normal API ownership/ancestor checks before any unlink. The managed
    // runtime directory is private to the current uid, which is the local trust boundary frozen
    // by the platform ADRs.
    crate::api::validate_api_config(config)?;

    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => {
            return Err(format!("{path:?} 仍有进程监听，拒绝自动处理"));
        }
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
        Err(error) => {
            return Err(format!("探测 {path:?} 失败，拒绝自动处理: {error}"));
        }
    }

    let current = std::fs::symlink_metadata(path)
        .map_err(|error| format!("重新确认 {path:?} 失败: {error}"))?;
    if !current.file_type().is_socket()
        || current.uid() != current_uid
        || current.dev() != metadata.dev()
        || current.ino() != metadata.ino()
    {
        return Err(format!("{path:?} 在探测期间被替换，拒绝自动处理"));
    }

    std::fs::remove_file(path).map_err(|error| format!("删除 stale socket {path:?} 失败: {error}"))
}

#[cfg(unix)]
struct ShutdownSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl ShutdownSignals {
    fn install() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }
    }
}

#[cfg(windows)]
struct ShutdownSignals {
    ctrl_c: tokio::signal::windows::CtrlC,
    ctrl_close: tokio::signal::windows::CtrlClose,
    ctrl_shutdown: tokio::signal::windows::CtrlShutdown,
}

#[cfg(windows)]
impl ShutdownSignals {
    fn install() -> std::io::Result<Self> {
        Ok(Self {
            ctrl_c: tokio::signal::windows::ctrl_c()?,
            ctrl_close: tokio::signal::windows::ctrl_close()?,
            ctrl_shutdown: tokio::signal::windows::ctrl_shutdown()?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.ctrl_c.recv() => {}
            _ = self.ctrl_close.recv() => {}
            _ = self.ctrl_shutdown.recv() => {}
        }
    }
}

fn terminal_text(value: &dyn fmt::Display) -> String {
    const MAX_CHARS: usize = 2_048;

    let mut output = String::with_capacity(MAX_CHARS);
    let mut truncated = false;
    for (count, character) in value.to_string().chars().enumerate() {
        if count == MAX_CHARS {
            truncated = true;
            break;
        }
        output.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    if truncated {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_rejects_relative_production_roots() {
        let root = Path::new("relative-root");
        let paths = AppPaths::from_roots(
            &root.join("config"),
            &root.join("data"),
            Some(&root.join("runtime")),
            Some(&root.join("state")),
            root,
        );
        let error = DaemonOptions::new(root.join("config").join(maild_brand::CONFIG_FILE_NAME), paths).unwrap_err();
        assert!(error.to_string().contains("必须是绝对路径"));
        assert!(error.is_configuration_stop());
    }

    #[test]
    fn signal_installation_failures_are_unexpected() {
        let error = DaemonError::unexpected("signal driver unavailable");
        assert!(!error.is_configuration_stop());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn injected_shutdown_disposes_store_and_unix_socket() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(
            &directory.path().join("config"),
            &directory.path().join("data"),
            Some(&directory.path().join("runtime")),
            Some(&directory.path().join("state")),
            directory.path(),
        );
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let config = paths.config_file.clone();
        std::fs::write(
            &config,
            r#"
                [plugins.store.config]
                db_path = "mail.sqlite3"
                [plugins.api.config]
                transport = "unix"
                listen = "mail.sock"
            "#,
        )
        .unwrap();
        let socket = paths.runtime_dir.join("mail.sock");
        let database = paths.data_dir.join("mail.sqlite3");
        let shutdown_socket = socket.clone();
        let shutdown = async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            while !shutdown_socket.exists() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "socket was not created"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };

        let options = DaemonOptions::new(config, paths).unwrap();
        run_daemon_until(options, shutdown).await.unwrap();
        assert!(database.exists());
        assert!(!socket.exists());
        drop(crate::store::Store::open(database.to_str().unwrap()).unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_agent_recovers_its_owned_refused_socket() {
        use std::os::unix::net::UnixListener;

        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(
            &directory.path().join("config"),
            &directory.path().join("data"),
            Some(&directory.path().join("runtime")),
            Some(&directory.path().join("state")),
            directory.path(),
        );
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::create_dir_all(&paths.runtime_dir).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&paths.runtime_dir, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let config = paths.config_file.clone();
        std::fs::write(
            &config,
            r#"
                [plugins.store.config]
                db_path = "mail.sqlite3"
                [plugins.api.config]
                transport = "unix"
                listen = "mail.sock"
            "#,
        )
        .unwrap();
        let socket = paths.runtime_dir.join("mail.sock");
        drop(UnixListener::bind(&socket).unwrap());

        let ready_socket = socket.clone();
        let shutdown = async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if tokio::net::UnixStream::connect(&ready_socket).await.is_ok() {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "managed Agent did not replace the refused socket"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };

        let options = DaemonOptions::new(config, paths)
            .unwrap()
            .with_managed_socket_recovery();
        run_daemon_until(options, shutdown).await.unwrap();
        assert!(!socket.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_accounts_are_rejected_before_store_or_socket_mounts() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(
            &directory.path().join("config"),
            &directory.path().join("data"),
            Some(&directory.path().join("runtime")),
            Some(&directory.path().join("state")),
            directory.path(),
        );
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let config = paths.config_file.clone();
        std::fs::write(
            &config,
            r#"
                [plugins.store.config]
                db_path = "mail.sqlite3"
                [plugins.api.config]
                transport = "unix"
                listen = "mail.sock"
                [accounts]
                account = "not-an-array"
            "#,
        )
        .unwrap();
        let socket = paths.runtime_dir.join("mail.sock");
        let database = paths.data_dir.join("mail.sqlite3");

        let options = DaemonOptions::new(config, paths).unwrap();
        let error = run_daemon_until(options, std::future::pending())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("accounts"));
        assert!(!database.exists());
        assert!(!socket.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_default_frontend_keeps_store_and_api_running() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(&directory.path().join("config"), &directory.path().join("data"),
            Some(&directory.path().join("runtime")), None, directory.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let config = paths.config_dir.join("fixture.toml");
        std::fs::write(&config, r#"
            [plugins.store.config]
            db_path = "mail.sqlite3"
            [plugins.api.config]
            transport = "unix"
            listen = "mail.sock"
            [plugins.frontend.config]
            default = "org.example.missing"
        "#).unwrap();
        let socket = paths.runtime_dir.join("mail.sock");
        let db = paths.data_dir.join("mail.sqlite3");
        let task = tokio::spawn(run_daemon_until(DaemonOptions::new(config, paths).unwrap().with_managed_agent_control(), std::future::pending()));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !socket.exists() { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        }).await.unwrap();
        let client = maild_client::RpcClient::new(maild_client::Endpoint::Unix { path: socket.clone() }).unwrap();
        let frontends = client.call("listFrontends", serde_json::json!({})).await.unwrap();
        assert_eq!(frontends["default"], crate::frontend::BUILTIN_RECOVERY_FRONTEND);
        assert!(client.call("status", serde_json::json!({})).await.is_ok());
        assert!(db.exists());
        assert_eq!(client.call("stopManagedAgent", serde_json::json!({})).await.unwrap()["stopping"], true);
        tokio::time::timeout(std::time::Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();
        assert!(crate::store::Store::open(db.to_str().unwrap()).is_ok());
        assert!(!socket.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_enabled_external_plugin_is_a_configuration_stop() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_roots(
            &directory.path().join("config"),
            &directory.path().join("data"),
            Some(&directory.path().join("runtime")),
            Some(&directory.path().join("state")),
            directory.path(),
        );
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let config = paths.config_file.clone();
        std::fs::write(
            &config,
            r#"
                [plugins.external.config]
                enabled = ["org.example.missing"]
            "#,
        )
        .unwrap();

        let options = DaemonOptions::new(config, paths).unwrap();
        let error = run_daemon_until(options, std::future::pending())
            .await
            .unwrap_err();
        assert!(error.is_configuration_stop());
        assert!(error.to_string().contains("external"));
    }
}
