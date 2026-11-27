//! Bounded lifecycle host for out-of-process protocol plugins.
//!
//! External plugins remain ordinary JSON-RPC clients. This module validates package manifests,
//! starts only an explicit allowlist of `supervised` executables, applies bounded exponential
//! restart, exposes coarse lifecycle observation and stops children during Cordis LIFO teardown.
//! Since ADR-0017/0018 the declared capabilities are the authorization table for authenticated
//! plugin principals; unauthenticated connections stay in the same-user trust domain.

use std::any::TypeId;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::{self, Read as _};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cordis::{Context, Plugin, PluginError, Service};
use maild_client::{ExternalPluginInfo, ExternalPluginsResult};
use serde::Deserialize;

use crate::plugin_identity::{IdentitySeam, PluginKind, TOKEN_ENV};

const MANIFEST_FILE: &str = "plugin.toml";
/// v1 是初版；v2 起可另外声明兜底 `description` 与 `[l10n.<tag>]`（ADR-0048 决策 5）。
const MANIFEST_SCHEMA_VERSION_V1: u32 = 1;
const MANIFEST_SCHEMA_VERSION_L10N: u32 = 2;
const API_PROTOCOL_VERSION: u32 = 1;
const MAX_PLUGINS: usize = 32;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ID_BYTES: usize = 128;
const MAX_NAME_BYTES: usize = 160;
const MAX_VERSION_BYTES: usize = 64;
const MAX_CAPABILITIES: usize = 32;
const MAX_SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize)]
pub struct ExternalConfig {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default = "default_restart_initial_secs")]
    pub restart_initial_secs: u64,
    #[serde(default = "default_restart_max_secs")]
    pub restart_max_secs: u64,
    #[serde(default = "default_max_restarts")]
    pub max_restarts: u32,
    #[serde(default = "default_healthy_reset_secs")]
    pub healthy_reset_secs: u64,
}

impl Default for ExternalConfig {
    fn default() -> Self {
        Self {
            enabled: Vec::new(),
            restart_initial_secs: default_restart_initial_secs(),
            restart_max_secs: default_restart_max_secs(),
            max_restarts: default_max_restarts(),
            healthy_reset_secs: default_healthy_reset_secs(),
        }
    }
}

fn default_restart_initial_secs() -> u64 {
    2
}

fn default_restart_max_secs() -> u64 {
    60
}

fn default_max_restarts() -> u32 {
    8
}

fn default_healthy_reset_secs() -> u64 {
    60
}

impl Service for ExternalConfig {
    fn name(&self) -> &'static str {
        "external.config"
    }
}

/// Platform-owned package/data directories and the exact config source inherited by supervised
/// clients. Package transactions never own `data_root`.
pub struct ExternalInstallRoot {
    pub root: PathBuf,
    pub data_root: PathBuf,
    pub config_path: PathBuf,
}

impl Service for ExternalInstallRoot {
    fn name(&self) -> &'static str {
        "external.install-root"
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalLifecycle {
    Supervised,
    Independent,
}

impl ExternalLifecycle {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Supervised => "supervised",
            Self::Independent => "independent",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ExternalCapability {
    EventsStream,
    MessagesRead,
    MessagesSend,
    SettingsRead,
    SettingsWrite,
    FrontendOpen,
    NotificationsRead,
    /// M4-05（ADR-0021）：可注册为 transport provider 并接收反向调用。
    TransportProvider,
    /// ADR-0050：可注册为外部翻译 provider 并接收 `translation/translate`、`translation/probe`
    /// 与 `translation/cancel` 反向调用。
    TranslationProvider,
    /// M4.5-G01（ADR-0030 决策 5）：信箱状态写回与离线队列裁决。自动归档/打标的规则插件
    /// 需要它，而作为**独立声明**开放，正是 ADR-0027 决策 9 当初要的——不要静默，要显式。
    ///
    /// 账户/地址/包管理三个词**不在** external 词表内：后台插件没有理由改账户配置，更没有
    /// 理由装下一个插件。禁止自我繁殖是这里唯一重要的性质。
    MailboxWrite,
    /// M4.5-G03（ADR-0034 决策 7）：读取本机草稿。
    DraftsRead,
    /// M4.5-G03：新建/修改/丢弃草稿。**故意与 `messages_send` 分开**——
    /// 「AI 起草回复、用户审阅后自己发送」这类插件要的正是「能写草稿但不能发信」。
    DraftsWrite,
}

impl ExternalCapability {
    fn as_str(self) -> &'static str {
        match self {
            Self::EventsStream => "events_stream",
            Self::MessagesRead => "messages_read",
            Self::MessagesSend => "messages_send",
            Self::SettingsRead => "settings_read",
            Self::SettingsWrite => "settings_write",
            Self::FrontendOpen => "frontend_open",
            Self::NotificationsRead => "notifications_read",
            Self::TransportProvider => "transport_provider",
            Self::TranslationProvider => "translation_provider",
            Self::MailboxWrite => "mailbox_write",
            Self::DraftsRead => "drafts_read",
            Self::DraftsWrite => "drafts_write",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalManifest {
    schema_version: u32,
    protocol_version: u32,
    id: String,
    name: String,
    version: String,
    lifecycle: ExternalLifecycle,
    capabilities: Vec<ExternalCapability>,
    #[serde(default)]
    executable: Option<PathBuf>,
    #[serde(default)]
    ui_executable: Option<PathBuf>,
    /// ADR-0048 决策 5：兜底描述，仅 `schema_version = 2` 起允许。
    #[serde(default)]
    description: Option<String>,
    /// ADR-0048 决策 5：各语言的名称/描述，仅 `schema_version = 2` 起允许。
    #[serde(default)]
    l10n: crate::manifest_l10n::ManifestL10n,
}

#[derive(Clone)]
pub(crate) struct ExternalRecord {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) lifecycle: ExternalLifecycle,
    pub(crate) capabilities: Vec<String>,
    pub(crate) package: PathBuf,
    pub(crate) executable: Option<PathBuf>,
    pub(crate) ui_executable: Option<PathBuf>,
    /// 校验后的 `description` / `l10n`，原样进 `listExternalPlugins` 与预览。
    pub(crate) text: crate::manifest_l10n::PackageText,
}

/// 三段数字版本（缺段补 0）；无法解析为 None（比较即失败，fail-closed）。
pub(crate) fn version_tuple(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = [0u64; 3];
    for (index, segment) in version.split('.').take(3).enumerate() {
        if segment.is_empty() || !segment.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        parts[index] = segment.parse::<u64>().ok()?;
    }
    Some((parts[0], parts[1], parts[2]))
}

/// 运行中的 supervised 子进程：独立 watch 通道支持单插件启停（ADR-0019 §5）。
pub(crate) struct RunningChild {
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

pub(crate) struct RunningUi {
    child: tokio::process::Child,
    containment: crate::containment::ChildContainment,
}

/// 已安装且声明 `translation_provider` 能力的外部包快照（供 `TranslationProviderRegistry` 合并）。
#[derive(Clone, Debug)]
pub(crate) struct InstalledTranslationPackage {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) l10n: maild_client::PackageL10n,
    pub(crate) version: String,
    pub(crate) lifecycle: String,
    pub(crate) enabled: bool,
    pub(crate) state: String,
    pub(crate) has_ui: bool,
}

impl ExternalRecord {
    fn has_translation_provider(&self) -> bool {
        self.capabilities
            .iter()
            .any(|cap| cap == "translation_provider")
    }
}

/// 只读生命周期目录 + 动态监督控制。内部可变：refresh 差异化启停、
/// enable/disable 即时生效（ADR-0019）。
pub struct ExternalPluginManager {
    core: Mutex<ManagerCore>,
}

struct ManagerCore {
    records: BTreeMap<String, ExternalRecord>,
    enabled: BTreeSet<String>,
    states: Arc<Mutex<BTreeMap<String, RuntimeState>>>,
    running: BTreeMap<String, RunningChild>,
    ui_running: BTreeMap<String, RunningUi>,
    install_root: PathBuf,
    data_root: PathBuf,
    config_path: PathBuf,
    policy: SupervisorPolicy,
    seam: Arc<IdentitySeam>,
    state_path: PathBuf,
    state: std::sync::Arc<crate::plugin_state::PluginState>,
    /// 宿主 Context：用于在生命周期变更（disable/remove/upgrade/rollback）时撤销
    /// `TranslationProviderRegistry` 活跃代次并广播 `translation/providers`。
    ctx: Option<Context>,
}

/// refresh 的差异化结果（ADR-0019 §4）。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RefreshReport {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spawned: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stopped: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restarted: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
}

impl ManagerCore {
    /// 回收自然退出的子进程句柄。
    fn prune_finished(&mut self) {
        let finished: Vec<String> = self
            .running
            .iter()
            .filter(|(_, child)| child.task.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in finished {
            self.running.remove(&id);
        }
    }

    fn spawn_child(core: &mut Self, record: &ExternalRecord) {
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            set_state(&core.states, &record.id, "blocked", None);
            return;
        };
        let task = runtime.spawn(supervise(
            record.clone(),
            core.config_path.clone(),
            core.data_root.clone(),
            core.policy,
            stop_rx,
            core.states.clone(),
            core.seam.clone(),
        ));
        core.running.insert(
            record.id.clone(),
            RunningChild {
                stop: stop_tx,
                task,
            },
        );
    }

    /// 取出子进程句柄（锁外等待其退出由调用方完成）。
    fn detach_child(core: &mut Self, id: &str) -> Option<RunningChild> {
        core.running.remove(id)
    }
}

impl ExternalPluginManager {
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let seam = IdentitySeam::open(dir.path().join("plugin_auth.toml"));
        let state_path = dir.path().join("plugin_state.toml");
        let state = Arc::new(crate::plugin_state::PluginState::load(&state_path));
        // TempDir 有意跨测试存续。
        std::mem::forget(dir);
        Self {
            core: Mutex::new(ManagerCore {
                records: BTreeMap::new(),
                enabled: BTreeSet::new(),
                states: Arc::new(Mutex::new(BTreeMap::new())),
                running: BTreeMap::new(),
                ui_running: BTreeMap::new(),
                install_root: PathBuf::new(),
                data_root: PathBuf::new(),
                config_path: PathBuf::new(),
                policy: SupervisorPolicy {
                    initial_delay: Duration::from_secs(2),
                    max_delay: Duration::from_secs(60),
                    healthy_reset: Duration::from_secs(60),
                    max_restarts: 8,
                },
                seam,
                state,
                state_path,
                ctx: None,
            }),
        }
    }

    /// 差异化重扫（ADR-0019 §4）：身份对账 + 按表启停；无重启风暴。
    pub async fn refresh(&self) -> Result<RefreshReport, String> {
        let (report, detached, detached_ui) = self.refresh_prepare()?;
        for mut child in detached {
            let _ = child.stop.send(true);
            let deadline = tokio::time::Instant::now() + MAX_SHUTDOWN_WAIT;
            if tokio::time::timeout_at(deadline, &mut child.task).await.is_err() {
                child.task.abort();
                let _ = child.task.await;
            }
        }
        Self::stop_ui_children(detached_ui).await;
        Ok(report)
    }

    /// 同步准备阶段：锁内完成 diff 与 spawn，被停的子进程句柄交还调用方
    /// 在锁外等待。apply（同步上下文）只用这个。
    pub(crate) fn refresh_prepare(
        &self,
    ) -> Result<(RefreshReport, Vec<RunningChild>, Vec<RunningUi>), String> {
        let mut report = RefreshReport::default();
        let mut detached: Vec<RunningChild> = Vec::new();
        let mut detached_ui = Vec::new();
        let mut to_revoke: Vec<(String, &'static str)> = Vec::new();
        let mut translation_changed = false;
        let ctx_clone: Option<Context>;
        {
            let mut core = self.core.lock().unwrap();
            ctx_clone = core.ctx.clone();
            if !core.seam.is_ready() || !core.state.is_ready() {
                // 停摆姿态下不做任何变更（与启动时一致：宁可不动也不无身份运行）。
                return Ok((report, detached, detached_ui));
            }
            core.prune_finished();
            let (records, skipped) = load_catalog(&core.install_root)?;
            if skipped > 0 {
                eprintln!(
                    "[maild/external] {} 个未知或损坏的 external package 已忽略",
                    skipped
                );
            }
            // 在身份/进程状态发生变化前先完成所有启用插件的数据目录门禁，避免
            // 部分 refresh 已停旧进程、另一插件才因不安全路径失败。
            for (id, record) in &records {
                if core.enabled.contains(id) && record.lifecycle == ExternalLifecycle::Supervised {
                    prepare_plugin_data_directory(&core.data_root, id)?;
                }
            }
            let ids: Vec<&str> = records.keys().map(String::as_str).collect();
            let _ = core.seam.reconcile(PluginKind::External, &ids);

            // 消失的 id：停止并移除观测；若是翻译插件则撤销活跃代次。
            let gone: Vec<String> = core
                .records
                .keys()
                .filter(|id| !records.contains_key(*id))
                .cloned()
                .collect();
            for id in &gone {
                if core
                    .records
                    .get(id)
                    .is_some_and(ExternalRecord::has_translation_provider)
                {
                    translation_changed = true;
                }
                to_revoke.push((id.clone(), "插件已移除"));
                core.enabled.remove(id);
                core.states.lock().unwrap().remove(id);
                report.removed.push(id.clone());
                if let Some(child) = ManagerCore::detach_child(&mut core, id) {
                    report.stopped.push(id.clone());
                    detached.push(child);
                }
                if let Some(child) = core.ui_running.remove(id) {
                    detached_ui.push(child);
                }
            }

            // 版本变化（升级/回滚，覆盖 supervised 与 independent）：停止旧进程并撤销活跃翻译代次。
            let changed: Vec<String> = records
                .iter()
                .filter(|(id, record)| {
                    core.records
                        .get(*id)
                        .is_some_and(|old| old.version != record.version)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in &changed {
                if core
                    .records
                    .get(id)
                    .is_some_and(ExternalRecord::has_translation_provider)
                    || records
                        .get(id)
                        .is_some_and(ExternalRecord::has_translation_provider)
                {
                    translation_changed = true;
                }
                to_revoke.push((id.clone(), "插件版本已变更（升级或回滚）"));
                if let Some(child) = ManagerCore::detach_child(&mut core, id) {
                    report.stopped.push(id.clone());
                    detached.push(child);
                }
                if let Some(child) = core.ui_running.remove(id) {
                    detached_ui.push(child);
                }
            }

            // 新增包或能力集合变化（如移除 translation_provider 能力）：撤销并触发清单刷新。
            for (id, record) in &records {
                match core.records.get(id) {
                    None => {
                        if record.has_translation_provider() {
                            translation_changed = true;
                        }
                        if record.lifecycle == ExternalLifecycle::Independent {
                            set_state(&core.states, id, "independent", None);
                        } else if !core.enabled.contains(id) {
                            set_state(&core.states, id, "disabled", None);
                        }
                    }
                    Some(old) => {
                        if old.has_translation_provider() != record.has_translation_provider() {
                            translation_changed = true;
                            if !record.has_translation_provider() {
                                to_revoke.push((id.clone(), "插件不再声明 translation_provider"));
                            }
                        } else if record.has_translation_provider()
                            && (old.name != record.name
                                || old.ui_executable.is_some() != record.ui_executable.is_some()
                                || old.lifecycle != record.lifecycle)
                        {
                            translation_changed = true;
                        }
                    }
                }
            }

            // 新出现或需要（重新）启动的 enabled supervised：spawn。
            for (id, record) in &records {
                let should_run =
                    core.enabled.contains(id) && record.lifecycle == ExternalLifecycle::Supervised;
                let is_running = core.running.contains_key(id);
                let was_known = core.records.contains_key(id);
                if should_run && !is_running {
                    set_state(&core.states, id, "starting", None);
                    ManagerCore::spawn_child(&mut core, record);
                    report.spawned.push(id.clone());
                    if was_known {
                        report.restarted.push(id.clone());
                    }
                }
            }

            core.records = records;
        }
        if let Some(ctx) = ctx_clone {
            let mut revoked_any = false;
            if let Some(registry) = ctx.get::<crate::translation_provider::TranslationProviderRegistry>()
            {
                for (id, reason) in &to_revoke {
                    if registry.revoke_plugin(id, reason) {
                        revoked_any = true;
                    }
                }
            }
            if translation_changed || revoked_any {
                crate::translation_provider::emit_providers_changed(&ctx);
            }
        }
        Ok((report, detached, detached_ui))
    }

    /// 启用/停用单个插件：写状态文件；supervised 即时启停，independent 只改意图；
    /// 停用时对 supervised 与 independent 一律立即撤销翻译 provider 活跃代次（ADR-0050）。
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        if let Some(mut child) = self.set_enabled_prepare(id, enabled)? {
            let _ = child.stop.send(true);
            let deadline = tokio::time::Instant::now() + MAX_SHUTDOWN_WAIT;
            let _ = tokio::time::timeout_at(deadline, &mut child.task).await;
        }
        Ok(())
    }

    /// set_enabled 的同步准备阶段（见 refresh_prepare 注释）。
    pub(crate) fn set_enabled_prepare(&self, id: &str, enabled: bool) -> Result<Option<RunningChild>, String> {
        let mut detached = None;
        let is_translation: bool;
        let ctx_clone: Option<Context>;
        {
            let mut core = self.core.lock().unwrap();
            ctx_clone = core.ctx.clone();
            if !core.state.is_ready() {
                return Err("插件启用状态停摆".into());
            }
            core.prune_finished();
            let record = core
                .records
                .get(id)
                .ok_or_else(|| format!("插件未安装: {id}"))?
                .clone();
            is_translation = record.has_translation_provider();
            if enabled && record.lifecycle == ExternalLifecycle::Supervised {
                prepare_plugin_data_directory(&core.data_root, id)?;
            }
            let mut next = core.enabled.clone();
            if enabled {
                next.insert(id.to_string());
            } else {
                next.remove(id);
            }
            core.state.store(&core.state_path, &next)?;
            core.enabled = next;
            // supervised 才有进程可管；independent 只落状态文件与观测快照，但停用后
            // 锁外仍必须撤销其活跃翻译 provider 代次。
            if record.lifecycle != ExternalLifecycle::Supervised {
                set_state(&core.states, id, "independent", None);
            } else if enabled {
                if !core.running.contains_key(id) {
                    if !core.seam.is_ready() {
                        set_state(&core.states, id, "blocked", None);
                        return Err("身份缝停摆，无法启动 supervised 插件".into());
                    }
                    set_state(&core.states, id, "starting", None);
                    ManagerCore::spawn_child(&mut core, &record);
                }
            } else {
                detached = ManagerCore::detach_child(&mut core, id);
                set_state(&core.states, id, "disabled", None);
            }
        }
        if let Some(ctx) = ctx_clone {
            let mut revoked = false;
            if !enabled {
                if let Some(registry) =
                    ctx.get::<crate::translation_provider::TranslationProviderRegistry>()
                {
                    revoked = registry.revoke_plugin(id, "翻译插件已停用");
                }
            }
            if is_translation || revoked {
                crate::translation_provider::emit_providers_changed(&ctx);
            }
        }
        Ok(detached)
    }

    /// LIFO teardown：反序停止全部子进程（总计上限等待）。
    pub async fn shutdown(&self) {
        let (children, ui_children, ctx_clone): (Vec<RunningChild>, Vec<RunningUi>, Option<Context>) = {
            let mut core = self.core.lock().unwrap();
            (
                std::mem::take(&mut core.running).into_values().collect(),
                std::mem::take(&mut core.ui_running).into_values().collect(),
                core.ctx.clone(),
            )
        };
        if let Some(ctx) = ctx_clone {
            if let Some(registry) =
                ctx.get::<crate::translation_provider::TranslationProviderRegistry>()
            {
                registry.revoke_all("daemon 正在关闭");
            }
        }
        let deadline = tokio::time::Instant::now() + MAX_SHUTDOWN_WAIT;
        for mut child in children {
            let _ = child.stop.send(true);
            if tokio::time::timeout_at(deadline, &mut child.task).await.is_err() {
                child.task.abort();
                let _ = child.task.await;
            }
        }
        Self::stop_ui_children(ui_children).await;
    }

    pub fn status(
        &self,
        identity: Option<&IdentitySeam>,
        providers: Option<&crate::provider::ProviderRegistry>,
    ) -> ExternalPluginsResult {
        let core = self.core.lock().unwrap();
        let states = core.states.lock().unwrap();
        ExternalPluginsResult {
            plugins: core
                .records
                .values()
                .map(|record| {
                    let runtime = states.get(&record.id).copied().unwrap_or(RuntimeState {
                        state: "unknown",
                        restart_count: 0,
                    });
                    ExternalPluginInfo {
                        id: record.id.clone(),
                        name: record.name.clone(),
                        description: record.text.description.clone(),
                        l10n: record.text.l10n.clone(),
                        version: record.version.clone(),
                        lifecycle: record.lifecycle.as_str().into(),
                        capabilities: record.capabilities.clone(),
                        enabled: core.enabled.contains(&record.id),
                        state: runtime.state.into(),
                        restart_count: runtime.restart_count,
                        has_identity: identity
                            .is_some_and(|seam| seam.contains(PluginKind::External, &record.id)),
                        denied_calls: identity
                            .map(|seam| seam.denied_total(&format!("plugin:{}", record.id)))
                            .unwrap_or(0),
                        provider_registered: providers
                            .is_some_and(|registry| registry.is_registered(&record.id)),
                        has_ui: record.ui_executable.is_some(),
                    }
                })
                .collect(),
        }
    }

    /// 某插件 manifest 声明的 capability 原词表；未在册返回 `None`
    /// （调用方按空表 fail-closed，ADR-0018 §2）。
    pub fn capabilities_of(&self, id: &str) -> Option<Vec<String>> {
        self.core
            .lock()
            .unwrap()
            .records
            .get(id)
            .map(|record| record.capabilities.clone())
    }

    /// 查询单个已安装且声明 `translation_provider` 能力的外部包快照。
    pub(crate) fn installed_translation_provider(
        &self,
        id: &str,
    ) -> Option<InstalledTranslationPackage> {
        let core = self.core.lock().unwrap();
        let record = core.records.get(id)?;
        if !record.has_translation_provider() {
            return None;
        }
        let states = core.states.lock().unwrap();
        let runtime = states.get(&record.id).copied().unwrap_or(RuntimeState {
            state: "unknown",
            restart_count: 0,
        });
        Some(InstalledTranslationPackage {
            id: record.id.clone(),
            name: record.name.clone(),
            description: record.text.description.clone(),
            l10n: record.text.l10n.clone(),
            version: record.version.clone(),
            lifecycle: record.lifecycle.as_str().into(),
            enabled: core.enabled.contains(&record.id),
            state: runtime.state.into(),
            has_ui: record.ui_executable.is_some(),
        })
    }

    /// 列出全部已安装且声明 `translation_provider` 能力的外部包（按 id 排序）。
    pub(crate) fn translation_provider_packages(&self) -> Vec<InstalledTranslationPackage> {
        let core = self.core.lock().unwrap();
        let states = core.states.lock().unwrap();
        core.records
            .values()
            .filter(|record| record.has_translation_provider())
            .map(|record| {
                let runtime = states.get(&record.id).copied().unwrap_or(RuntimeState {
                    state: "unknown",
                    restart_count: 0,
                });
                InstalledTranslationPackage {
                    id: record.id.clone(),
                    name: record.name.clone(),
                    description: record.text.description.clone(),
                    l10n: record.text.l10n.clone(),
                    version: record.version.clone(),
                    lifecycle: record.lifecycle.as_str().into(),
                    enabled: core.enabled.contains(&record.id),
                    state: runtime.state.into(),
                    has_ui: record.ui_executable.is_some(),
                }
            })
            .collect()
    }

    /// Launch a package-declared native UI at most once. The executable is revalidated at the
    /// moment of launch so a post-scan package replacement cannot escape the package boundary.
    pub fn open_ui(&self, id: &str) -> Result<maild_client::OpenExternalPluginUiResult, String> {
        validate_id(id)?;
        let mut core = self.core.lock().unwrap();
        if let Some(running) = core.ui_running.get_mut(id) {
            match running.child.try_wait() {
                Ok(None) => {
                    return Ok(maild_client::OpenExternalPluginUiResult {
                        id: id.to_owned(),
                        launched: false,
                        already_running: true,
                    })
                }
                Ok(Some(_)) => running.containment.terminate_after_exit(),
                Err(_) => return Err("无法检查插件界面进程状态".into()),
            }
        }
        core.ui_running.remove(id);
        let record = core
            .records
            .get(id)
            .ok_or_else(|| format!("插件未安装: {id}"))?
            .clone();
        let resolved = record
            .ui_executable
            .as_ref()
            .ok_or_else(|| "插件没有图形化管理界面".to_string())?;
        let relative = resolved
            .strip_prefix(&record.package)
            .map_err(|_| "插件界面路径越过 package".to_string())?;
        let executable = resolve_executable(&record.package, relative)?;
        let data_dir = prepare_plugin_data_directory(&core.data_root, id)?;
        let mut command = tokio::process::Command::new(executable);
        command
            .env_clear()
            .env(maild_paths::CONFIG_ENV, &core.config_path)
            .env(maild_paths::PLUGIN_DATA_ENV, data_dir)
            .current_dir(&record.package)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::containment::ChildContainment::prepare(&mut command);
        let mut child = command
            .spawn()
            .map_err(|_| "无法启动插件图形化管理界面".to_string())?;
        let Some(containment) = crate::containment::ChildContainment::arm(&child) else {
            let _ = child.start_kill();
            return Err("无法装配插件界面进程 containment".into());
        };
        core.ui_running
            .insert(id.to_owned(), RunningUi { child, containment });
        Ok(maild_client::OpenExternalPluginUiResult {
            id: id.to_owned(),
            launched: true,
            already_running: false,
        })
    }

    async fn stop_ui_children(children: Vec<RunningUi>) {
        let mut tasks = tokio::task::JoinSet::new();
        for mut running in children {
            tasks.spawn(async move {
                running
                    .containment
                    .stop(&mut running.child, MAX_SHUTDOWN_WAIT)
                    .await
            });
        }
        while tasks.join_next().await.is_some() {}
    }
}

#[derive(Clone, Copy)]
struct RuntimeState {
    state: &'static str,
    restart_count: u32,
}

impl Service for ExternalPluginManager {
    fn name(&self) -> &'static str {
        "external.manager"
    }
}

#[derive(Clone, Copy)]
struct SupervisorPolicy {
    initial_delay: Duration,
    max_delay: Duration,
    healthy_reset: Duration,
    max_restarts: u32,
}

impl SupervisorPolicy {
    fn from_config(config: &ExternalConfig) -> Self {
        Self {
            initial_delay: Duration::from_secs(config.restart_initial_secs),
            max_delay: Duration::from_secs(config.restart_max_secs),
            healthy_reset: Duration::from_secs(config.healthy_reset_secs),
            max_restarts: config.max_restarts,
        }
    }
}

pub(crate) fn validate_config(config: &ExternalConfig) -> Result<(), String> {
    if config.restart_initial_secs == 0
        || config.restart_initial_secs > 300
        || config.restart_max_secs < config.restart_initial_secs
        || config.restart_max_secs > 3_600
        || config.max_restarts == 0
        || config.max_restarts > 100
        || config.healthy_reset_secs == 0
        || config.healthy_reset_secs > 86_400
    {
        return Err("external restart 策略越界".into());
    }
    if config.enabled.len() > MAX_PLUGINS {
        return Err("external enabled 数量超过上限".into());
    }
    let mut enabled = HashSet::new();
    for id in &config.enabled {
        validate_id(id)?;
        if !enabled.insert(id) {
            return Err("external enabled 不允许重复 ID".into());
        }
    }
    Ok(())
}

fn prepare_plugin_data_directory(root: &Path, id: &str) -> Result<PathBuf, String> {
    // Keep the path derivation tied to the same validated identifier used by package manifests;
    // never accept an opaque path from a plugin or configuration file.
    validate_id(id)?;
    ensure_private_data_directory(root)?;
    let path = root.join(id);
    ensure_private_data_directory(&path)?;
    Ok(path)
}

fn ensure_private_data_directory(path: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                match builder.create(path) {
                    Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                        .map_err(|error| {
                            format!("创建 external plugin 私有数据目录失败: {error}")
                        })?,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(format!("创建 external plugin 私有数据目录失败: {error}"))
                    }
                }
            }
            #[cfg(not(unix))]
            match fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!("创建 external plugin 私有数据目录失败: {error}"))
                }
            }
            fs::symlink_metadata(path)
                .map_err(|error| format!("检查 external plugin 私有数据目录失败: {error}"))?
        }
        Err(error) => return Err(format!("检查 external plugin 私有数据目录失败: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "external plugin 私有数据目录必须是非符号链接目录: {path:?}"
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let uid = unsafe { libc::geteuid() };
        if metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
            return Err(format!(
                "external plugin 私有数据目录必须由当前 uid 所有且权限为 0700: {path:?}"
            ));
        }
    }
    Ok(())
}

pub struct ExternalPlugin;

impl Plugin for ExternalPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![
            TypeId::of::<ExternalConfig>(),
            TypeId::of::<ExternalInstallRoot>(),
        ]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let config = ctx.get::<ExternalConfig>().expect("inject 就绪");
        validate_config(&config)?;
        let install = ctx.get::<ExternalInstallRoot>().expect("inject 就绪");
        let seam = ctx
            .get::<IdentitySeam>()
            .ok_or_else(|| "external 需要 plugin.identity-seam 服务".to_string())?;

        // 启用状态（ADR-0019 §3）：状态文件缺失时以 TOML allowlist 为一次性种子；
        // 此后 TOML 数组被忽略，状态文件是唯一权威。
        let state_path = crate::plugin_state::state_path_for(&install.config_path);
        if !state_path.exists() {
            let seed: BTreeSet<String> = config.enabled.iter().cloned().collect();
            if let Err(reason) =
                crate::plugin_state::PluginState::Ready(seed).store(&state_path, &{
                    let back: BTreeSet<String> = config.enabled.iter().cloned().collect();
                    back
                })
            {
                eprintln!("[maild/external] 启用状态种子导入失败: {reason}");
            }
        }
        let state = Arc::new(crate::plugin_state::PluginState::load(&state_path));

        let (records, skipped) = load_catalog(&install.root)?;
        if skipped > 0 {
            eprintln!(
                "[maild/external] {} 个未知或损坏的 external package 已忽略",
                skipped
            );
        }
        let enabled = state.enabled();
        for id in &enabled {
            let record = records
                .get(id)
                .ok_or_else(|| format!("external enabled 未安装或损坏: {id}"))?;
            if record.lifecycle == ExternalLifecycle::Supervised && record.executable.is_none() {
                return Err(format!("external enabled 不是 supervised executable: {id}").into());
            }
        }

        // 初始观测快照：refresh 会把 enabled 的 spawn 出来。
        let identity_ready = seam.is_ready();
        if !enabled.is_empty() && !identity_ready {
            eprintln!(
                "[maild/external] 插件身份缝不可用，supervised 插件拒绝启动: {}",
                enabled.clone().into_iter().collect::<Vec<_>>().join(", ")
            );
        }
        let states = Arc::new(Mutex::new(BTreeMap::new()));
        {
            let mut snapshot = states.lock().unwrap();
            for record in records.values() {
                snapshot.insert(
                    record.id.clone(),
                    RuntimeState {
                        state: if record.lifecycle == ExternalLifecycle::Independent {
                            "independent"
                        } else if enabled.contains(&record.id) {
                            if identity_ready {
                                "starting"
                            } else {
                                "blocked"
                            }
                        } else {
                            "disabled"
                        },
                        restart_count: 0,
                    },
                );
            }
        }

        let manager = Arc::new(ExternalPluginManager {
            core: Mutex::new(ManagerCore {
                records,
                enabled,
                states,
                running: BTreeMap::new(),
                ui_running: BTreeMap::new(),
                install_root: install.root.clone(),
                data_root: install.data_root.clone(),
                config_path: install.config_path.clone(),
                policy: SupervisorPolicy::from_config(&config),
                seam,
                state,
                state_path,
                ctx: Some(ctx.clone()),
            }),
        });
        manager.refresh_prepare()?;
        ctx.provide::<ExternalPluginManager>(manager.clone());
        ctx.effect_async(async move {
            manager.shutdown().await;
        });
        Ok(())
    }
}

async fn supervise(
    record: ExternalRecord,
    config_path: PathBuf,
    data_root: PathBuf,
    policy: SupervisorPolicy,
    mut stop: tokio::sync::watch::Receiver<bool>,
    states: Arc<Mutex<BTreeMap<String, RuntimeState>>>,
    seam: Arc<IdentitySeam>,
) {
    let executable = record
        .executable
        .as_ref()
        .expect("supervised record has executable");
    let mut consecutive = 0u32;
    let mut delay = policy.initial_delay;
    loop {
        if *stop.borrow() {
            set_state(&states, &record.id, "stopped", None);
            return;
        }
        let data_dir = match prepare_plugin_data_directory(&data_root, &record.id) {
            Ok(path) => path,
            Err(reason) => {
                eprintln!(
                    "[maild/external] 私有数据目录不可用，supervised 插件停止: {}: {reason}",
                    record.id
                );
                set_state(&states, &record.id, "blocked", None);
                return;
            }
        };
        // 每 spawn 轮换凭证（ADR-0017）：旧 token 对新连接立即失效，会话归因
        // 绑定到 spawn 代。轮换失败 = 身份缝停摆，宁可停止也不无身份运行。
        set_state(&states, &record.id, "starting", None);
        let token = match seam.rotate(PluginKind::External, &record.id) {
            Ok(token) => token,
            Err(_) => {
                eprintln!(
                    "[maild/external] 凭证轮换失败，supervised 插件停止: {}",
                    record.id
                );
                set_state(&states, &record.id, "blocked", None);
                return;
            }
        };
        let mut command = tokio::process::Command::new(executable);
        command
            .env_clear()
            .env(maild_paths::CONFIG_ENV, &config_path)
            .env(TOKEN_ENV, &token)
            .env(maild_paths::PLUGIN_DATA_ENV, &data_dir)
            .current_dir(&record.package)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::containment::ChildContainment::prepare(&mut command);
        let child = command.spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(_) => {
                if !schedule_restart(&states, &record.id, &mut consecutive, policy.max_restarts) {
                    return;
                }
                if wait_or_stop(delay, &mut stop).await {
                    set_state(&states, &record.id, "stopped", None);
                    return;
                }
                delay = delay.saturating_mul(2).min(policy.max_delay);
                continue;
            }
        };
        // ADR-0020：装配失败 = 拒绝无 containment 运行（宁可退避重试）。
        let containment = match crate::containment::ChildContainment::arm(&child) {
            Some(containment) => containment,
            None => {
                set_state(&states, &record.id, "blocked", None);
                eprintln!(
                    "[maild/external] 无法装配进程 containment，supervised 插件停止: {}",
                    record.id
                );
                return;
            }
        };
        set_state(&states, &record.id, "running", None);
        let started = tokio::time::Instant::now();
        enum Exit {
            /// 外部停止信号：终态，写状态后返回。
            Stopped(bool),
            /// 直接子进程自行退出；(exit 0, 是否组内干净)。非零退出走重启阶梯，
            /// 不在此写终态（backoff 会立即覆盖）。
            Exited(bool, bool),
        }
        let exit = tokio::select! {
            status = child.wait() => {
                // 契约要求前台运行：自然退出也要清扫组内违约后代（ADR-0020 §3）。
                let clean = containment.sweep_after_exit().await;
                Exit::Exited(matches!(&status, Ok(s) if s.success()), clean)
            }
            _ = stop.changed() => {
                let clean = containment.stop(&mut child, MAX_SHUTDOWN_WAIT).await;
                Exit::Stopped(clean)
            }
        };
        match exit {
            // 外部停止与主动退出（exit 0）都是终态；clean=false 时写入 `leaked`。
            Exit::Stopped(clean) | Exit::Exited(true, clean) => {
                finalize_stop(&states, &record.id, clean);
                return;
            }
            Exit::Exited(false, _) => {}
        }
        if started.elapsed() >= policy.healthy_reset {
            consecutive = 0;
            delay = policy.initial_delay;
        }
        if !schedule_restart(&states, &record.id, &mut consecutive, policy.max_restarts) {
            return;
        }
        if wait_or_stop(delay, &mut stop).await {
            set_state(&states, &record.id, "stopped", None);
            return;
        }
        delay = delay.saturating_mul(2).min(policy.max_delay);
    }
}

fn schedule_restart(
    states: &Mutex<BTreeMap<String, RuntimeState>>,
    id: &str,
    consecutive: &mut u32,
    max_restarts: u32,
) -> bool {
    if *consecutive >= max_restarts {
        set_state(states, id, "failed", None);
        return false;
    }
    *consecutive = consecutive.saturating_add(1);
    set_state(states, id, "backoff", Some(1));
    true
}

/// 停止收尾（ADR-0020 §4）：组内清空写 `stopped`，仍有成员写 `leaked` 并告警——
/// 把 daemonize 违约从静默变成可观测信号。
fn finalize_stop(
    states: &Mutex<BTreeMap<String, RuntimeState>>,
    id: &str,
    clean: bool,
) {
    if clean {
        set_state(states, id, "stopped", None);
        return;
    }
    eprintln!("[maild/external] 插件停止后进程组仍有存活成员（leaked）: {id}");
    set_state(states, id, "leaked", None);
}

fn set_state(
    states: &Mutex<BTreeMap<String, RuntimeState>>,
    id: &str,
    state: &'static str,
    increment_restarts: Option<u32>,
) {
    let mut states = states.lock().unwrap();
    let entry = states.entry(id.into()).or_insert(RuntimeState {
        state,
        restart_count: 0,
    });
    entry.state = state;
    entry.restart_count = entry
        .restart_count
        .saturating_add(increment_restarts.unwrap_or(0));
}

async fn wait_or_stop(delay: Duration, stop: &mut tokio::sync::watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        _ = stop.changed() => true,
    }
}

fn load_catalog(root: &Path) -> Result<(BTreeMap<String, ExternalRecord>, usize), String> {
    let metadata =
        fs::symlink_metadata(root).map_err(|_| "external 安装目录不可检查".to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("external 安装目录类型无效".into());
    }
    validate_owned_entry(&metadata)?;
    let mut packages = Vec::with_capacity(MAX_PLUGINS);
    for entry in fs::read_dir(root).map_err(|_| "external 安装目录不可读取".to_string())? {
        let entry = entry.map_err(|_| "external 安装目录扫描失败".to_string())?;
        if entry.file_name().to_string_lossy().starts_with('.') { continue; }
        if packages.len() == MAX_PLUGINS {
            return Err("external package 数量超过上限".into());
        }
        packages.push(entry);
    }
    packages.sort_by_key(std::fs::DirEntry::file_name);
    let mut records = BTreeMap::new();
    let mut skipped = 0usize;
    for package in packages {
        // 点前缀目录是事务 staging/残留（.incoming/.removed/.rollback），永不激活。
        if package.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        match inspect_package(&package.path()) {
            Ok(record) => {
                records.insert(record.id.clone(), record);
            }
            Err(_) => skipped = skipped.saturating_add(1),
        }
    }
    Ok((records, skipped))
}

/// staging 宽松校验：目录名是随机名，manifest id 相等性留给发布位；
/// 其余（版本、capability、executable 解析）全量执行。
pub(crate) fn inspect_staged(package: &Path) -> Result<ExternalRecord, String> {
    inspect_package_inner(package, None)
}

/// 校验一个包目录并产出记录（发布位的严格入口，ADR-0019 §3）。
pub(crate) fn inspect_package(package: &Path) -> Result<ExternalRecord, String> {
    inspect_package_inner(package, package.file_name().and_then(|n| n.to_str()))
}

fn inspect_package_inner(
    package: &Path,
    expected_id: Option<&str>,
) -> Result<ExternalRecord, String> {
    let metadata = fs::symlink_metadata(package)
        .map_err(|_| "external package cannot be inspected".to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("external package must be a directory".into());
    }
    validate_owned_entry(&metadata)?;
    let manifest_text = read_manifest(&package.join(MANIFEST_FILE))?;
    let manifest: ExternalManifest =
        toml::from_str(&manifest_text).map_err(|_| "external manifest is invalid".to_string())?;
    let text = validate_manifest(&manifest, expected_id)?;
    let executable = match (manifest.lifecycle, manifest.executable) {
        (ExternalLifecycle::Supervised, Some(path)) => Some(resolve_executable(package, &path)?),
        (ExternalLifecycle::Supervised, None) => {
            return Err("supervised external plugin requires executable".into())
        }
        (ExternalLifecycle::Independent, None) => None,
        (ExternalLifecycle::Independent, Some(_)) => {
            return Err("independent external plugin must not declare executable".into())
        }
    };
    let ui_executable = manifest
        .ui_executable
        .map(|path| resolve_executable(package, &path))
        .transpose()?;
    let mut capabilities = manifest
        .capabilities
        .into_iter()
        .map(|capability| capability.as_str().to_string())
        .collect::<Vec<_>>();
    capabilities.sort();
    Ok(ExternalRecord {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        lifecycle: manifest.lifecycle,
        capabilities,
        package: package.to_path_buf(),
        executable,
        ui_executable,
        text,
    })
}

fn read_manifest(path: &Path) -> Result<String, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "external manifest is missing".to_string())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err("external manifest type or size is invalid".into());
    }
    validate_owned_entry(&metadata)?;
    let file =
        fs::File::open(path).map_err(|_| "external manifest cannot be opened".to_string())?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "external manifest cannot be read".to_string())?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("external manifest is too large".into());
    }
    String::from_utf8(bytes).map_err(|_| "external manifest is not UTF-8".into())
}

/// 校验通过时返回校验后的 `description` / `l10n`（ADR-0048 决策 5）。
fn validate_manifest(
    manifest: &ExternalManifest,
    package_id: Option<&str>,
) -> Result<crate::manifest_l10n::PackageText, String> {
    if !matches!(
        manifest.schema_version,
        MANIFEST_SCHEMA_VERSION_V1 | MANIFEST_SCHEMA_VERSION_L10N
    ) || manifest.protocol_version != API_PROTOCOL_VERSION
    {
        return Err("external manifest version is unsupported".into());
    }
    validate_id(&manifest.id)?;
    if package_id.is_some_and(|package_id| manifest.id != package_id) {
        return Err("external manifest id must match its directory".into());
    }
    if !valid_text(&manifest.name, MAX_NAME_BYTES)
        || manifest.version.is_empty()
        || manifest.version.len() > MAX_VERSION_BYTES
        || !manifest
            .version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
    {
        return Err("external name or version is invalid".into());
    }
    if manifest.capabilities.is_empty()
        || manifest.capabilities.len() > MAX_CAPABILITIES
        || manifest
            .capabilities
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len()
            != manifest.capabilities.len()
    {
        return Err("external capabilities are invalid".into());
    }
    // v1 包写了这两个键直接拒：旧 daemon 本来也会因未知键拒绝它，行为与新旧 daemon 一致。
    if manifest.schema_version < MANIFEST_SCHEMA_VERSION_L10N
        && (manifest.description.is_some() || !manifest.l10n.is_empty())
    {
        return Err("external description and l10n require manifest schema_version 2".into());
    }
    crate::manifest_l10n::validate(manifest.description.as_deref(), &manifest.l10n, MAX_NAME_BYTES)
        .map_err(|reason| format!("external {reason}"))
}

fn validate_id(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let lowered = value.to_ascii_lowercase();
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || value.contains("..")
        || lowered == "builtin"
        || lowered.starts_with("builtin.")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("external plugin id 无效".into());
    }
    Ok(())
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn resolve_executable(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("external executable must remain inside its package".into());
    }
    let mut current = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|_| "external executable path is missing".to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("external executable path must not contain symlinks".into());
        }
        validate_owned_entry(&metadata)?;
        let is_final = index + 1 == components.len();
        if !is_final && !metadata.is_dir() {
            return Err("external executable parent must be a directory".into());
        }
        if is_final && (!metadata.is_file() || !is_executable(&metadata)) {
            return Err("external executable target type is invalid".into());
        }
    }
    Ok(current)
}

#[cfg(unix)]
fn validate_owned_entry(metadata: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
        return Err(
            "external package entries must be current-user owned and not writable by others".into(),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_owned_entry(_metadata: &fs::Metadata) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0030 决策 5：`mailbox_write` 进 external 词表（规则/自动化插件需要它，且是**显式
    /// 声明**——正是 ADR-0027 决策 9 当初要的）；账户/地址/包管理三个词不进，manifest 直接拒。
    /// 禁止自我繁殖是这里唯一重要的性质。
    #[cfg(unix)]
    #[test]
    fn external_vocabulary_takes_mailbox_write_but_no_management_words() {
        use std::os::unix::fs::PermissionsExt as _;
        fn package_with(root: &Path, id: &str, capabilities: &str) -> Result<ExternalRecord, String> {
            let package = root.join(id);
            fs::create_dir(&package).unwrap();
            fs::set_permissions(&package, fs::Permissions::from_mode(0o700)).unwrap();
            let manifest = package.join(MANIFEST_FILE);
            fs::write(
                &manifest,
                format!(
                    "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Vocab fixture\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [{capabilities}]\n"
                ),
            )
            .unwrap();
            fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600)).unwrap();
            inspect_package(&package)
        }

        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let accepted = package_with(root.path(), "org.example.filer", "\"messages_read\", \"mailbox_write\"")
            .expect("规则插件可以声明信箱写回");
        assert!(accepted.capabilities.iter().any(|cap| cap.as_str() == "mailbox_write"));

        // ADR-0034 决策 7：草稿两个词对后台插件开放——「AI 起草回复、用户自己发送」
        // 要的正是「能写草稿但不能发信」，那是 external 主体的正当用途。
        let drafter = package_with(root.path(), "org.example.drafter", "\"drafts_read\", \"drafts_write\"")
            .expect("起草插件可以声明草稿读写");
        assert!(drafter.capabilities.iter().any(|cap| cap.as_str() == "drafts_write"));
        assert!(
            !drafter.capabilities.iter().any(|cap| cap.as_str() == "messages_send"),
            "草稿词不得顺带把发信授权带进来"
        );

        for word in ["accounts_manage", "addresses_manage", "plugins_manage"] {
            let id = format!("org.example.{}", word.replace('_', ""));
            assert!(
                package_with(root.path(), &id, &format!("\"{word}\"")).is_err(),
                "external manifest 不应接受 {word}"
            );
        }
    }

    #[test]
    fn restart_policy_and_enabled_ids_are_bounded() {
        let mut config = ExternalConfig::default();
        assert!(validate_config(&config).is_ok());
        config.restart_initial_secs = 0;
        assert!(validate_config(&config).is_err());
        config.restart_initial_secs = 2;
        config.enabled = vec!["org.example.plugin".into(), "org.example.plugin".into()];
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn plugin_data_directory_is_stable_and_rejects_non_directory() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("plugin-data");
        let first = prepare_plugin_data_directory(&data_root, "org.example.stable").unwrap();
        let second = prepare_plugin_data_directory(&data_root, "org.example.stable").unwrap();
        assert_eq!(first, second);
        assert!(first.is_dir());

        fs::write(data_root.join("org.example.file"), b"not a directory").unwrap();
        let error = prepare_plugin_data_directory(&data_root, "org.example.file").unwrap_err();
        assert!(error.contains("非符号链接目录"));
    }

    #[cfg(unix)]
    #[test]
    fn plugin_data_directory_is_0700_and_rejects_symlink_or_broad_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let root = secure_tempdir();
        let data_root = root.path().join("plugin-data");
        let private = prepare_plugin_data_directory(&data_root, "org.example.private").unwrap();
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let target = root.path().join("outside");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&target, data_root.join("org.example.link")).unwrap();
        let error = prepare_plugin_data_directory(&data_root, "org.example.link").unwrap_err();
        assert!(error.contains("非符号链接目录"));

        let broad = data_root.join("org.example.broad");
        fs::create_dir(&broad).unwrap();
        fs::set_permissions(&broad, fs::Permissions::from_mode(0o755)).unwrap();
        let error = prepare_plugin_data_directory(&data_root, "org.example.broad").unwrap_err();
        assert!(error.contains("权限为 0700"));
    }

    #[cfg(unix)]
    fn secure_tempdir() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[cfg(unix)]
    fn write_package(
        root: &Path,
        id: &str,
        lifecycle: &str,
        executable: Option<&str>,
        ui_executable: Option<&str>,
    ) {
        use std::os::unix::fs::PermissionsExt as _;
        let package = root.join(id);
        fs::create_dir(&package).unwrap();
        fs::set_permissions(&package, fs::Permissions::from_mode(0o700)).unwrap();
        let executable_line = executable
            .map(|path| format!("executable = \"{path}\"\n"))
            .unwrap_or_default();
        let ui_executable_line = ui_executable
            .map(|path| format!("ui_executable = \"{path}\"\n"))
            .unwrap_or_default();
        fs::write(
            package.join(MANIFEST_FILE),
            format!(
                "schema_version = 1\nprotocol_version = 1\nid = \"{id}\"\nname = \"Test plugin\"\nversion = \"1.0.0\"\nlifecycle = \"{lifecycle}\"\ncapabilities = [\"messages_read\"]\n{executable_line}{ui_executable_line}"
            ),
        )
        .unwrap();
        fs::set_permissions(
            package.join(MANIFEST_FILE),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn catalog_lists_independent_and_rejects_escape() {
        let root = secure_tempdir();
        write_package(
            root.path(),
            "org.example.independent",
            "independent",
            None,
            None,
        );
        write_package(
            root.path(),
            "org.example.escape",
            "supervised",
            Some("../outside"),
            None,
        );
        let (records, skipped) = load_catalog(root.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(skipped, 1);
        assert_eq!(
            records["org.example.independent"].lifecycle,
            ExternalLifecycle::Independent
        );
    }

    /// ADR-0048 决策 5：`description` 与 `[l10n.<tag>]` 只在 plugin.toml v2 起允许，校验后
    /// 原样进 `listExternalPlugins`；v1 包照旧，JSON 里不出现这两个键。
    #[cfg(unix)]
    #[test]
    fn l10n_requires_schema_two_and_passes_through_listing() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = secure_tempdir();
        let install_root = root.path().join("plugins");
        fs::create_dir(&install_root).unwrap();
        fs::set_permissions(&install_root, fs::Permissions::from_mode(0o700)).unwrap();
        write_package(&install_root, "org.example.v1", "independent", None, None);
        for (id, schema_version, extra) in [
            (
                "org.example.l10n",
                2,
                "description = \"Spam filter\"\n[l10n.\"zh-hans-cn\"]\nname = \"垃圾邮件过滤\"\n",
            ),
            ("org.example.v1l10n", 1, "[l10n.en]\nname = \"n\"\n"),
            ("org.example.v1desc", 1, "description = \"d\"\n"),
            ("org.example.future", 3, ""),
        ] {
            let package = install_root.join(id);
            fs::create_dir(&package).unwrap();
            fs::set_permissions(&package, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(
                package.join(MANIFEST_FILE),
                format!(
                    "schema_version = {schema_version}\nprotocol_version = 1\nid = \"{id}\"\nname = \"Test plugin\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"messages_read\"]\n{extra}"
                ),
            )
            .unwrap();
            fs::set_permissions(package.join(MANIFEST_FILE), fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let (records, skipped) = load_catalog(&install_root).unwrap();
        assert_eq!(skipped, 3, "v1 写 l10n/description 与未知的 v3 都整包拒绝");
        let manager = manager_over(root.path(), install_root, records);
        let listed = serde_json::to_value(manager.status(None, None)).unwrap();
        let plugins = listed["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 2);
        let localized = plugins.iter().find(|p| p["id"] == "org.example.l10n").unwrap();
        assert_eq!(localized["name"], "Test plugin");
        assert_eq!(localized["description"], "Spam filter");
        assert_eq!(
            localized["l10n"],
            serde_json::json!({ "zh-Hans-CN": { "name": "垃圾邮件过滤" } })
        );
        let plain = plugins.iter().find(|p| p["id"] == "org.example.v1").unwrap();
        assert!(plain.get("l10n").is_none() && plain.get("description").is_none());
    }

    #[cfg(unix)]
    fn ui_manager(root: &Path, id: &str, body: &str) -> ExternalPluginManager {
        use std::os::unix::fs::PermissionsExt as _;

        let install_root = root.join("plugins");
        fs::create_dir(&install_root).unwrap();
        fs::set_permissions(&install_root, fs::Permissions::from_mode(0o700)).unwrap();
        write_package(
            &install_root,
            id,
            "independent",
            None,
            Some("bin/ui.sh"),
        );
        let package = install_root.join(id);
        fs::create_dir(package.join("bin")).unwrap();
        fs::set_permissions(package.join("bin"), fs::Permissions::from_mode(0o700)).unwrap();
        let executable = package.join("bin/ui.sh");
        fs::write(&executable, body).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let (records, skipped) = load_catalog(&install_root).unwrap();
        assert_eq!(skipped, 0);
        manager_over(root, install_root, records)
    }

    /// 已扫描好的目录之上的管理器：全部记录都是 `independent` 状态，不启动任何子进程。
    #[cfg(unix)]
    fn manager_over(
        root: &Path,
        install_root: PathBuf,
        records: BTreeMap<String, ExternalRecord>,
    ) -> ExternalPluginManager {
        let state_path = root.join("plugin_state.toml");
        let state = Arc::new(crate::plugin_state::PluginState::load(&state_path));
        let seam = IdentitySeam::open(root.join("plugin_auth.toml"));
        let states = Arc::new(Mutex::new(
            records
                .keys()
                .map(|id| {
                    (
                        id.clone(),
                        RuntimeState {
                            state: "independent",
                            restart_count: 0,
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        ));
        ExternalPluginManager {
            core: Mutex::new(ManagerCore {
                records,
                enabled: BTreeSet::new(),
                states,
                running: BTreeMap::new(),
                ui_running: BTreeMap::new(),
                install_root,
                data_root: root.join("plugin-data"),
                config_path: root.join(maild_brand::CONFIG_FILE_NAME),
                policy: SupervisorPolicy {
                    initial_delay: Duration::from_millis(5),
                    max_delay: Duration::from_millis(10),
                    healthy_reset: Duration::from_secs(60),
                    max_restarts: 2,
                },
                seam,
                state,
                state_path,
                ctx: None,
            }),
        }
    }

    #[cfg(unix)]
    fn process_exists(pid: u32) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[cfg(unix)]
    #[test]
    fn translation_provider_capability_and_builtin_namespace_reservation() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = secure_tempdir();
        let pkg = root.path().join("org.example.translator");
        fs::create_dir(&pkg).unwrap();
        fs::set_permissions(&pkg, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            pkg.join(MANIFEST_FILE),
            "schema_version = 2\nprotocol_version = 1\nid = \"org.example.translator\"\nname = \"Translator\"\ndescription = \"External translator\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"translation_provider\"]\n[l10n.\"zh-Hans\"]\nname = \"翻译插件\"\n",
        )
        .unwrap();
        fs::set_permissions(pkg.join(MANIFEST_FILE), fs::Permissions::from_mode(0o600)).unwrap();
        let record = inspect_package(&pkg).expect("schema v2 translation_provider 应校验通过");
        assert!(record
            .capabilities
            .contains(&"translation_provider".to_string()));
        assert_eq!(record.text.description.as_deref(), Some("External translator"));

        // builtin.* 保留命名空间必须拒绝。
        for bad_id in ["builtin.openai_compatible", "Builtin.Custom", "builtin"] {
            let bad_pkg = root.path().join(bad_id);
            fs::create_dir(&bad_pkg).unwrap();
            fs::set_permissions(&bad_pkg, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(
                bad_pkg.join(MANIFEST_FILE),
                format!(
                    "schema_version = 2\nprotocol_version = 1\nid = \"{bad_id}\"\nname = \"Hijack\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [\"translation_provider\"]\n"
                ),
            )
            .unwrap();
            fs::set_permissions(bad_pkg.join(MANIFEST_FILE), fs::Permissions::from_mode(0o600))
                .unwrap();
            assert!(
                inspect_package(&bad_pkg).is_err(),
                "external manifest 不得注册保留命名空间 {bad_id}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn ui_manifest_is_listed_and_rejects_escape_or_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let root = secure_tempdir();
        let manager = ui_manager(root.path(), "org.example.ui", "#!/bin/sh\nexit 0\n");
        let listing = manager.status(None, None);
        assert_eq!(listing.plugins.len(), 1);
        assert!(listing.plugins[0].has_ui);

        let escape_root = root.path().join("escape-packages");
        fs::create_dir(&escape_root).unwrap();
        fs::set_permissions(&escape_root, fs::Permissions::from_mode(0o700)).unwrap();
        write_package(
            &escape_root,
            "org.example.ui-escape",
            "independent",
            None,
            Some("../outside"),
        );
        let error = match inspect_package(&escape_root.join("org.example.ui-escape")) {
            Ok(_) => panic!("UI executable escaping its package must be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("inside its package"));

        write_package(
            &escape_root,
            "org.example.ui-link",
            "independent",
            None,
            Some("bin/ui"),
        );
        let link_package = escape_root.join("org.example.ui-link");
        fs::create_dir(link_package.join("bin")).unwrap();
        fs::set_permissions(link_package.join("bin"), fs::Permissions::from_mode(0o700)).unwrap();
        let outside = root.path().join("outside-ui");
        fs::write(&outside, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&outside, link_package.join("bin/ui")).unwrap();
        let error = match inspect_package(&link_package) {
            Ok(_) => panic!("symlinked UI executable must be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("symlinks"));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn ui_launch_is_single_instance_reaps_exit_and_shutdown_kills_child() {
        let root = secure_tempdir();
        let manager = ui_manager(
            root.path(),
            "org.example.ui",
            "#!/bin/sh\n/bin/sleep 30 &\necho $! > descendant.pid\nwait\n",
        );
        let first = manager.open_ui("org.example.ui").unwrap();
        assert!(first.launched && !first.already_running);
        let pid = manager.core.lock().unwrap().ui_running["org.example.ui"]
            .child
            .id()
            .unwrap();
        let descendant_path = root.path().join("plugins/org.example.ui/descendant.pid");
        // The shell's `>` creates the file before `echo` writes it, so wait for a
        // complete PID rather than mere existence (empty reads raced under load).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let descendant_pid = loop {
            if let Some(pid) = fs::read_to_string(&descendant_path)
                .ok()
                .filter(|text| text.ends_with('\n'))
                .and_then(|text| text.trim().parse::<u32>().ok())
            {
                break pid;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "UI descendant pid was not recorded"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let second = manager.open_ui("org.example.ui").unwrap();
        assert!(!second.launched && second.already_running);
        manager.shutdown().await;
        assert!(!process_exists(pid));
        assert!(!process_exists(descendant_pid));

        let exit_root = secure_tempdir();
        let manager = ui_manager(
            exit_root.path(),
            "org.example.ui-exit",
            "#!/bin/sh\nexit 0\n",
        );
        assert!(manager.open_ui("org.example.ui-exit").unwrap().launched);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let result = manager.open_ui("org.example.ui-exit").unwrap();
            if result.launched {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "自然退出的 UI 未被回收"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn disabled_package_upgrade_stops_old_ui_child() {
        let root = secure_tempdir();
        let manager = ui_manager(
            root.path(),
            "org.example.ui-upgrade",
            "#!/bin/sh\nexec /bin/sleep 30\n",
        );
        manager.open_ui("org.example.ui-upgrade").unwrap();
        let pid = manager.core.lock().unwrap().ui_running["org.example.ui-upgrade"]
            .child
            .id()
            .unwrap();
        let manifest = root
            .path()
            .join("plugins/org.example.ui-upgrade/plugin.toml");
        let upgraded = fs::read_to_string(&manifest)
            .unwrap()
            .replace("version = \"1.0.0\"", "version = \"2.0.0\"");
        fs::write(&manifest, upgraded).unwrap();

        manager.refresh().await.unwrap();

        assert!(!process_exists(pid));
        assert!(!manager
            .core
            .lock()
            .unwrap()
            .ui_running
            .contains_key("org.example.ui-upgrade"));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn package_removal_stops_ui_child() {
        let root = secure_tempdir();
        let manager = ui_manager(
            root.path(),
            "org.example.ui-remove",
            "#!/bin/sh\nexec /bin/sleep 30\n",
        );
        manager.open_ui("org.example.ui-remove").unwrap();
        let pid = manager.core.lock().unwrap().ui_running["org.example.ui-remove"]
            .child
            .id()
            .unwrap();
        fs::remove_dir_all(root.path().join("plugins/org.example.ui-remove")).unwrap();
        let report = manager.refresh().await.unwrap();
        assert_eq!(report.removed, vec!["org.example.ui-remove"]);
        assert!(!process_exists(pid));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn crashing_supervised_process_has_bounded_restart() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = secure_tempdir();
        let executable = root.path().join("fail.sh");
        fs::write(&executable, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let record = ExternalRecord {
            id: "org.example.fail".into(),
            name: "Fail".into(),
            version: "1.0.0".into(),
            lifecycle: ExternalLifecycle::Supervised,
            capabilities: vec!["messages_read".into()],
            package: root.path().into(),
            executable: Some(executable),
            ui_executable: None,
            text: Default::default(),
        };
        let states = Arc::new(Mutex::new(BTreeMap::new()));
        states.lock().unwrap().insert(
            record.id.clone(),
            RuntimeState {
                state: "starting",
                restart_count: 0,
            },
        );
        let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let seam = IdentitySeam::open(root.path().join("plugin_auth.toml"));
        let _ = seam.reconcile(PluginKind::External, &[&record.id]);
        supervise(
            record,
            root.path().join(maild_brand::CONFIG_FILE_NAME),
            root.path().join("plugin-data"),
            SupervisorPolicy {
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(10),
                healthy_reset: Duration::from_secs(60),
                max_restarts: 2,
            },
            stop_rx,
            states.clone(),
            seam,
        )
        .await;
        let state = states.lock().unwrap()["org.example.fail"];
        assert_eq!(state.state, "failed");
        assert_eq!(state.restart_count, 2);
    }
}
