//! Frontend activation broker and inspected out-of-process frontend catalog.
//!
//! The headless daemon owns navigation intent routing. Frontends are protocol consumers: a click
//! or plugin action enqueues a bounded one-shot [`OpenIntent`], activates the selected frontend,
//! and the frontend claims the intent through the authenticated local API. Sensitive mail data
//! never crosses argv, URLs, environment variables or process listings.

mod selection;
#[cfg(test)]
mod recovery_tests;
mod lease;
pub(crate) use lease::FrontendDirectoryLease;

use std::any::TypeId;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use cordis::{Context, Plugin, PluginError, Service};
pub use maild_client::OpenIntent;
use maild_client::{FrontendInfo, FrontendsResult, PackageL10nEntry};
use serde::Deserialize;

use crate::notify::Notifier;
use crate::plugin_identity::{IdentitySeam, PluginKind};

pub use maild_client::{
    is_builtin_frontend, BUILTIN_RECOVERY_FRONTEND, DEFAULT_FRONTEND, LEGACY_EGUI_FRONTEND,
};

const MANIFEST_FILE: &str = "frontend.toml";
/// v1 只有路由 `capabilities`；v2 起可另外声明纯授权的 `permissions`（ADR-0030 决策 3）；
/// v3 起可另外声明兜底 `description` 与 `[l10n.<tag>]`（ADR-0048 决策 5）。
const MANIFEST_SCHEMA_VERSION_V1: u32 = 1;
const MANIFEST_SCHEMA_VERSION_PERMISSIONS: u32 = 2;
const MANIFEST_SCHEMA_VERSION_L10N: u32 = 3;
pub(crate) const API_PROTOCOL_VERSION: u32 = 1;
const MAX_PENDING_INTENTS: usize = 64;
const MAX_FRONTENDS: usize = 64;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_FRONTEND_ID_BYTES: usize = 128;
const MAX_NAME_BYTES: usize = 160;
const MAX_VERSION_BYTES: usize = 64;
const MAX_INTENT_FIELD_BYTES: usize = 320;
const MAX_CAPABILITIES: usize = 16;
const MAX_PERMISSIONS: usize = 16;

#[derive(Clone, Debug, Deserialize)]
pub struct FrontendConfig {
    #[serde(default = "default_frontend")]
    pub default: String,
}

impl Default for FrontendConfig {
    fn default() -> Self {
        Self {
            default: default_frontend(),
        }
    }
}

fn default_frontend() -> String {
    DEFAULT_FRONTEND.into()
}

/// Read-time migration of the removed egui shell (ADR-0039). User TOML and selection files are
/// never rewritten; an explicit choice persisted later simply replaces the legacy value.
pub(crate) fn migrate_default(id: &str) -> &str {
    if id == LEGACY_EGUI_FRONTEND {
        DEFAULT_FRONTEND
    } else {
        id
    }
}

impl Service for FrontendConfig {
    fn name(&self) -> &'static str {
        "frontend.config"
    }
}

/// Platform-owned directory containing `<id>/frontend.toml` packages.
pub struct FrontendInstallRoot(pub PathBuf);

impl Service for FrontendInstallRoot {
    fn name(&self) -> &'static str {
        "frontend.install-root"
    }
}

/// Preparation rejected an ancestor/permission boundary. Absence of a root alone also describes
/// unconfigured development contexts, so failure needs an explicit fail-closed marker.
pub(crate) struct FrontendDirectoryUnavailable;
impl Service for FrontendDirectoryUnavailable {
    fn name(&self) -> &'static str { "frontend.directory-unavailable" }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum FrontendLifecycle {
    OnDemand,
    Independent,
}

impl FrontendLifecycle {
    fn as_str(self) -> &'static str {
        match self {
            Self::OnDemand => "on_demand",
            Self::Independent => "independent",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "snake_case")]
enum FrontendCapability {
    Inbox,
    Message,
    Compose,
    Settings,
    Background,
}

impl FrontendCapability {
    const ALL: [Self; 5] = [
        Self::Inbox,
        Self::Message,
        Self::Compose,
        Self::Settings,
        Self::Background,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Message => "message",
            Self::Compose => "compose",
            Self::Settings => "settings",
            Self::Background => "background",
        }
    }

    fn for_intent(intent: &OpenIntent) -> Option<Self> {
        match intent {
            OpenIntent::Inbox { .. } => Some(Self::Inbox),
            OpenIntent::Message { .. } => Some(Self::Message),
            OpenIntent::Compose => Some(Self::Compose),
            OpenIntent::Settings => Some(Self::Settings),
            OpenIntent::Background => Some(Self::Background),
            OpenIntent::Unknown => None,
        }
    }
}

/// 纯授权词（ADR-0030 决策 2/4）：与路由 `capabilities` 分开声明，缺省空表。
///
/// 路由词回答「这个 intent 交给谁」，授权词回答「它能调哪些方法」。v1 把两者并在一个字段是
/// 欠账——往 `message` 挂写权限会让每个已安装的只读前端静默获得删邮件的能力。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum FrontendPermission {
    /// 信箱状态写回与离线队列裁决（ADR-0027 的五个方法）。
    MailboxWrite,
    /// 账户配置面。秘密另有 origin 门禁，持有本词也提交不了密码（ADR-0030 决策 7）。
    AccountsManage,
    /// 别名与一次性地址（ADR-0022）。
    AddressesManage,
    /// 受管包生命周期。**声明它等于声明「我可以代表用户安装任意受管包」。**
    PluginsManage,
    /// 系统通知授权状态（ADR-0031 决策 8）。ADR-0018 当年说前端 manifest 表达不了它、
    /// 「未来以 additive 清单版本扩展」——schema 2 的 permissions 正是那条路。
    NotificationsRead,
    /// M4.5-G03（ADR-0034 决策 7）：读取本机草稿。**不含**收件箱——草稿是用户还没决定
    /// 要不要发的字，只申请「读邮件」的主体没有理由自动看到它。
    DraftsRead,
    /// M4.5-G03：新建/修改/丢弃草稿。与 `messages_send` 互不包含：能改草稿的主体可以在
    /// 用户按下发送之前换掉收件人，能发信的主体并没有理由翻草稿箱。
    DraftsWrite,
}

impl FrontendPermission {
    fn as_str(self) -> &'static str {
        match self {
            Self::MailboxWrite => "mailbox_write",
            Self::AccountsManage => "accounts_manage",
            Self::AddressesManage => "addresses_manage",
            Self::PluginsManage => "plugins_manage",
            Self::NotificationsRead => "notifications_read",
            Self::DraftsRead => "drafts_read",
            Self::DraftsWrite => "drafts_write",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum LaunchKind {
    MacosApplication,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchManifest {
    kind: LaunchKind,
    path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontendManifest {
    schema_version: u32,
    protocol_version: u32,
    id: String,
    name: String,
    version: String,
    lifecycle: FrontendLifecycle,
    capabilities: Vec<FrontendCapability>,
    /// ADR-0030：纯授权词，仅 `schema_version = 2` 起允许非空。
    #[serde(default)]
    permissions: Vec<FrontendPermission>,
    #[serde(default)]
    launch: Option<LaunchManifest>,
    /// ADR-0048 决策 5：兜底描述，仅 `schema_version = 3` 起允许。
    #[serde(default)]
    description: Option<String>,
    /// ADR-0048 决策 5：各语言的名称/描述，仅 `schema_version = 3` 起允许。
    #[serde(default)]
    l10n: crate::manifest_l10n::ManifestL10n,
}

#[derive(Clone, Debug)]
enum LaunchTarget {
    Recovery,
    MacosApplication(PathBuf),
    Independent,
}

#[derive(Clone, Debug)]
struct FrontendRecord {
    info: FrontendInfo,
    capabilities: HashSet<FrontendCapability>,
    permissions: HashSet<FrontendPermission>,
    launch: LaunchTarget,
}

/// The native launcher's listing name, worded like its window title in each kernel language.
fn recovery_name(locale: maild_l10n::Locale) -> String {
    (locale.messages().launcher.window_title)(maild_brand::PRODUCT_NAME)
}

impl FrontendRecord {
    /// The native launcher: always present, routes only `background` and authorizes no API.
    fn recovery() -> Self {
        Self {
            info: FrontendInfo {
                id: BUILTIN_RECOVERY_FRONTEND.into(),
                name: recovery_name(maild_l10n::Locale::default()),
                description: None,
                l10n: maild_l10n::Locale::ALL
                    .into_iter()
                    .map(|locale| {
                        let entry = PackageL10nEntry { name: Some(recovery_name(locale)), description: None };
                        (locale.tag().to_owned(), entry)
                    })
                    .collect(),
                version: env!("CARGO_PKG_VERSION").into(),
                lifecycle: FrontendLifecycle::OnDemand.as_str().into(),
                capabilities: vec!["background".into()],
                permissions: Vec::new(),
                builtin: true,
                has_identity: false,
                denied_calls: 0,
            },
            capabilities: HashSet::from([FrontendCapability::Background]),
            permissions: HashSet::new(),
            launch: LaunchTarget::Recovery,
        }
    }

    fn supports(&self, intent: &OpenIntent) -> bool {
        FrontendCapability::for_intent(intent)
            .is_some_and(|capability| self.capabilities.contains(&capability))
    }
}

struct FrontendCatalog {
    records: BTreeMap<String, FrontendRecord>,
    skipped: usize,
}

impl FrontendCatalog {
    fn recovery_only() -> Self {
        Self { records: BTreeMap::from([(BUILTIN_RECOVERY_FRONTEND.into(), FrontendRecord::recovery())]), skipped: 0 }
    }

    fn load(root: Option<&Path>) -> Result<Self, String> {
        let mut records = BTreeMap::new();
        records.insert(BUILTIN_RECOVERY_FRONTEND.into(), FrontendRecord::recovery());
        let Some(root) = root else {
            return Ok(Self {
                records,
                skipped: 0,
            });
        };
        let root_metadata =
            fs::symlink_metadata(root).map_err(|_| "frontend 安装目录不可检查".to_string())?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err("frontend 安装目录类型无效".into());
        }
        validate_owned_entry(&root_metadata)?;
        let mut packages = Vec::with_capacity(MAX_FRONTENDS);
        for entry in fs::read_dir(root).map_err(|_| "frontend 安装目录不可读取".to_string())?
        {
            let entry = entry.map_err(|_| "frontend 安装目录扫描失败".to_string())?;
            if entry.file_name().to_string_lossy().starts_with('.') { continue; }
            if packages.len() == MAX_FRONTENDS {
                return Err("frontend package 数量超过上限".into());
            }
            packages.push(entry);
        }
        packages.sort_by_key(std::fs::DirEntry::file_name);
        let mut skipped = 0usize;
        for package in packages {
            // 点前缀目录是事务 staging/残留（.incoming/.removed/.rollback），永不激活。
            if package.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            match load_package_at(&package.path(), package.path().file_name().and_then(|n| n.to_str())) {
                Ok(record) if !is_builtin_frontend(&record.info.id) => {
                    records.insert(record.info.id.clone(), record);
                }
                Ok(_) | Err(_) => skipped = skipped.saturating_add(1),
            }
        }
        Ok(Self { records, skipped })
    }
}

trait FrontendLauncher: Send + Sync + 'static {
    /// `true` means a platform activation request was issued; independent frontends return false.
    /// `is_default` is `true` only when `frontend` is currently the catalog's default frontend.
    fn activate(&self, frontend: &FrontendRecord, is_default: bool) -> Result<bool, &'static str>;
}

struct PlatformLauncher;

impl FrontendLauncher for PlatformLauncher {
    fn activate(&self, frontend: &FrontendRecord, is_default: bool) -> Result<bool, &'static str> {
        match &frontend.launch {
            LaunchTarget::Recovery => maild_client::open_native_recovery().map(|()| true),
            LaunchTarget::MacosApplication(path) => {
                let host = single_dock_host(&frontend.info.id, is_default, || {
                    platform_single_dock_host(&frontend.info.id, path)
                });
                activate_application(host.as_deref().unwrap_or(path)).map(|()| true)
            }
            LaunchTarget::Independent => Ok(false),
        }
    }
}

fn is_single_dock_hostable(id: &str) -> bool {
    id == crate::recovery::BUNDLED_FRONTEND_ID || id == crate::recovery::STUDIO_FRONTEND_ID
}

/// ADR-0044: a bundled frontend (`BUNDLED_FRONTEND_ID` or `STUDIO_FRONTEND_ID`) is hosted in place
/// by the top-level app bundle (one Dock identity) ONLY when it is the current default frontend;
/// explicit requests for a non-default frontend or any third-party frontend activate their own app.
fn single_dock_host(
    id: &str,
    is_default: bool,
    host: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    (is_default && is_single_dock_hostable(id))
        .then(host)
        .flatten()
}

/// `Some(<APP_BUNDLE_NAME>)` only for a real user session Agent inside a complete app bundle that
/// carries the matching in-place UI binary (`Contents/MacOS/<UI_BIN>` or
/// `Contents/MacOS/featherpost-studio`), with no connection overrides, and whose installed app
/// still passes basic checks. For Studio, the installed package version must also match the
/// bundled payload (exact match or same-version stale build that the launcher will resync); a
/// different standalone Studio version activates its own installed `.app` directly so the launcher
/// and Agent never loop.
#[cfg(target_os = "macos")]
fn platform_single_dock_host(id: &str, installed: &Path) -> Option<PathBuf> {
    use maild_client::NativeAppLayout;
    if NativeAppLayout::has_custom_connection() || !maild_client::system_home() {
        return None;
    }
    let layout = NativeAppLayout::discover().ok()??;
    if id == crate::recovery::STUDIO_FRONTEND_ID {
        layout.studio_ui_executable().ok()?;
        macos::validate_installed(installed).ok()?;
        let installed_package = installed.parent()?;
        let payload = layout.studio_frontend_payload();
        match inspect_installed_frontend(id, installed_package, &payload).ok()? {
            InstalledFrontendMatch::MatchesBundled | InstalledFrontendMatch::Stale => {}
            InstalledFrontendMatch::DifferentVersion | InstalledFrontendMatch::Unreferenced => {
                return None;
            }
        }
    } else if id == crate::recovery::BUNDLED_FRONTEND_ID {
        layout.ui_executable().ok()?;
        macos::validate_installed(installed).ok()?;
    } else {
        return None;
    }
    Some(layout.app_bundle().to_path_buf())
}

#[cfg(not(target_os = "macos"))]
fn platform_single_dock_host(_id: &str, _installed: &Path) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "macos")]
fn activate_application(path: &Path) -> Result<(), &'static str> {
    macos::activate_application(path)
}

#[cfg(not(target_os = "macos"))]
fn activate_application(_path: &Path) -> Result<(), &'static str> {
    Err("macOS frontend application is unsupported on this platform")
}

#[cfg(target_os = "macos")]
mod macos;

struct PendingIntent {
    token: u64,
    frontend: String,
    intent: OpenIntent,
}

struct PendingState {
    next_token: u64,
    items: VecDeque<PendingIntent>,
}

/// 入队回调（ADR-0045）：参数是目标 frontend id。插件装配时注入为
/// `ctx.emit::<FrontendIntentQueued>`，卸载时撤回。
pub type IntentWaker = Arc<dyn Fn(&str) + Send + Sync>;

/// Bounded one-shot navigation queue plus an inspected frontend catalog.
pub struct FrontendBroker {
    directory_available: AtomicBool,
    catalog_authoritative: AtomicBool,
    selection: Mutex<selection::Selection>,
    pending: Mutex<PendingState>,
    /// 目录快照；事务 RPC 经 [`FrontendBroker::reload_from_disk`] 热替换（ADR-0019）。
    records: Mutex<BTreeMap<String, FrontendRecord>>,
    launcher: Arc<dyn FrontendLauncher>,
    /// 每次入队后调用（锁外），把「有意图待领」实时推给已连接的目标前端（ADR-0045）。
    intent_waker: Mutex<Option<IntentWaker>>,
}

impl Service for FrontendBroker {
    fn name(&self) -> &'static str {
        "frontend.broker"
    }
}

impl FrontendBroker {
    pub fn new(config: &FrontendConfig) -> Result<Self, String> {
        Self::with_install_root(config, None)
    }

    pub fn with_install_root(
        config: &FrontendConfig,
        install_root: Option<&Path>,
    ) -> Result<Self, String> {
        validate_config(config)?;
        let catalog = match FrontendCatalog::load(install_root) {
            Ok(catalog) => catalog,
            Err(_) => {
                eprintln!("[maild/frontend] 前端目录不可用，保留原生修复入口");
                return Self::without_directory(config);
            }
        };
        if catalog.skipped > 0 {
            eprintln!(
                "[maild/frontend] {} 个未知或损坏的 frontend package 已忽略",
                catalog.skipped
            );
        }
        let selection = selection::Selection::load(&config.default, install_root);
        let broker = Self::from_catalog(config, catalog, Arc::new(PlatformLauncher));
        *broker.selection.lock().unwrap() = selection;
        Ok(broker)
    }

    /// Re-read safely; never keep launching stale paths after a failed scan. Desired selection
    /// stays intact so reinstalling its package restores the user's preference.
    pub fn reload_from_disk(&self, install_root: Option<&Path>) -> Result<(), String> {
        if !self.directory_available.load(Ordering::Acquire) { return Err("前端目录未取得安全访问权，请重启后台后重试".into()); }
        let catalog = match FrontendCatalog::load(install_root) {
            Ok(catalog) => catalog,
            Err(error) => {
                self.catalog_authoritative.store(false, Ordering::Release);
                *self.records.lock().unwrap() = FrontendCatalog::recovery_only().records;
                return Err(error);
            }
        };
        *self.records.lock().unwrap() = catalog.records;
        self.catalog_authoritative.store(true, Ordering::Release);
        Ok(())
    }

    fn without_directory(config: &FrontendConfig) -> Result<Self, String> {
        validate_config(config)?;
        let broker = Self::from_catalog(config, FrontendCatalog::recovery_only(), Arc::new(PlatformLauncher));
        broker.directory_available.store(false, Ordering::Release);
        broker.catalog_authoritative.store(false, Ordering::Release);
        broker.selection.lock().unwrap().freeze();
        Ok(broker)
    }

    fn from_catalog(
        config: &FrontendConfig,
        catalog: FrontendCatalog,
        launcher: Arc<dyn FrontendLauncher>,
    ) -> Self {
        Self {
            directory_available: AtomicBool::new(true),
            catalog_authoritative: AtomicBool::new(true),
            selection: Mutex::new(selection::Selection::load(&config.default, None)),
            pending: Mutex::new(PendingState {
                next_token: 1,
                items: VecDeque::with_capacity(MAX_PENDING_INTENTS),
            }),
            records: Mutex::new(catalog.records),
            launcher,
            intent_waker: Mutex::new(None),
        }
    }

    /// 注入（`Some`）或撤回（`None`）入队唤醒回调（ADR-0045）。
    pub fn set_intent_waker(&self, waker: Option<IntentWaker>) {
        *self.intent_waker.lock().unwrap() = waker;
    }

    #[cfg(test)]
    fn with_launcher(config: &FrontendConfig, launcher: Arc<dyn FrontendLauncher>) -> Self {
        Self::from_catalog(
            config,
            FrontendCatalog::load(None).expect("builtin catalog"),
            launcher,
        )
    }

    /// Queue an intent for the configured frontend, then request foreground activation.
    pub fn open(&self, intent: OpenIntent) -> Result<bool, &'static str> {
        self.open_for(None, intent)
    }

    /// Queue for an explicit installed frontend, or use the configured default when omitted.
    /// If the default does not implement a route, the bundled recovery frontend handles it.
    pub fn open_for(
        &self,
        requested: Option<&str>,
        intent: OpenIntent,
    ) -> Result<bool, &'static str> {
        validate_intent(&intent).map_err(|_| "open intent is invalid")?;
        // A legacy egui request is an alias, not an explicit choice: it degrades to background.
        let explicit = requested.is_some_and(|id| id != LEGACY_EGUI_FRONTEND);
        let default = self.default_frontend();
        // The removed egui shell's id stays reserved; old callers reach native recovery.
        let requested = match requested {
            Some(LEGACY_EGUI_FRONTEND) => BUILTIN_RECOVERY_FRONTEND,
            Some(id) => id,
            None => &default,
        };
        validate_frontend_id(requested).map_err(|_| "frontend id is invalid")?;
        // 目录快照锁内解析目标：默认前端不支持该路由时直接改投内建恢复前端；
        // activate 在锁外执行。
        let selected = {
            let records = self.records.lock().unwrap();
            let selected = records.get(requested)
                .or_else(|| (!explicit).then(|| records.get(BUILTIN_RECOVERY_FRONTEND)).flatten())
                .ok_or("frontend is not installed")?.clone();
            if !selected.supports(&intent) {
                if explicit {
                    return Err("frontend does not support this intent");
                }
                records
                    .get(BUILTIN_RECOVERY_FRONTEND)
                    .expect("builtin recovery frontend is always installed")
                    .clone()
            } else {
                selected
            }
        };

        let routed_intent = if selected.info.id == BUILTIN_RECOVERY_FRONTEND { OpenIntent::Background } else { intent };
        let token = self.enqueue(&selected.info.id, routed_intent);
        let is_default = selected.info.id == default;
        match self.launcher.activate(&selected, is_default) {
            Ok(requested) => Ok(requested),
            Err(_) if !explicit && selected.info.id != BUILTIN_RECOVERY_FRONTEND => {
                self.remove_pending(token);
                let builtin = FrontendRecord::recovery();
                self.enqueue(BUILTIN_RECOVERY_FRONTEND, OpenIntent::Background);
                Ok(self.launcher.activate(&builtin, false).is_ok())
            }
            Err(_) => Ok(false),
        }
    }

    fn enqueue(&self, frontend: &str, intent: OpenIntent) -> u64 {
        let token = {
            let mut pending = self.pending.lock().unwrap();
            if pending.items.len() == MAX_PENDING_INTENTS {
                pending.items.pop_front();
            }
            let token = pending.next_token;
            pending.next_token = pending.next_token.wrapping_add(1).max(1);
            pending.items.push_back(PendingIntent {
                token,
                frontend: frontend.into(),
                intent,
            });
            token
        };
        // ADR-0045：队列锁已释放再唤醒——监听器同步执行，可能回头 claim。
        let waker = self.intent_waker.lock().unwrap().clone();
        if let Some(waker) = waker {
            waker(frontend);
        }
        token
    }

    fn remove_pending(&self, token: u64) {
        let mut pending = self.pending.lock().unwrap();
        if let Some(index) = pending.items.iter().position(|item| item.token == token) {
            pending.items.remove(index);
        }
    }

    pub fn claim(&self, frontend: &str) -> Result<Option<OpenIntent>, &'static str> {
        validate_frontend_id(frontend).map_err(|_| "frontend id is invalid")?;
        if !self.records.lock().unwrap().contains_key(frontend) {
            return Err("frontend is not installed");
        }
        let mut pending = self.pending.lock().unwrap();
        let Some(index) = pending
            .items
            .iter()
            .position(|item| item.frontend == frontend)
        else {
            return Ok(None);
        };
        Ok(pending.items.remove(index).map(|item| item.intent))
    }

    pub fn frontends(&self, identity: Option<&IdentitySeam>) -> FrontendsResult {
        let selection = self.selection.lock().unwrap();
        let records = self.records.lock().unwrap();
        FrontendsResult {
            default: if selection.is_healthy() && records.contains_key(&selection.id) { selection.id.clone() } else { BUILTIN_RECOVERY_FRONTEND.into() },
            frontends: records
                .values()
                .map(|record| {
                    let mut info = record.info.clone();
                    info.has_identity = identity
                        .is_some_and(|seam| seam.contains(PluginKind::Frontend, &info.id));
                    info.denied_calls = identity
                        .map(|seam| seam.denied_total(&format!("frontend:{}", info.id)))
                        .unwrap_or(0);
                    info
                })
                .collect(),
        }
    }

    /// 某前端的有效 API capability 表：**route 翻译所得 ∪ `permissions` 声明所得**
    /// （ADR-0018 §2 固定翻译 + ADR-0030 决策 2）；未在册返回 `None`（调用方按空表
    /// fail-closed）。
    pub fn api_capabilities_of(&self, id: &str) -> Option<Vec<&'static str>> {
        let record = self.records.lock().unwrap().get(id)?.clone();
        let mut caps = Vec::new();
        for route in &record.capabilities {
            match route {
                FrontendCapability::Inbox | FrontendCapability::Message => {
                    caps.push("messages_read");
                }
                FrontendCapability::Compose => caps.push("messages_send"),
                FrontendCapability::Settings => {
                    caps.push("settings_read");
                    caps.push("settings_write");
                }
                // background 是路由语义，不授权任何 API。
                FrontendCapability::Background => {}
            }
        }
        // 授权词是自己的名字，没有翻译表——路由需要翻译是因为它本来在回答另一个问题。
        caps.extend(record.permissions.iter().map(|p| p.as_str()));
        // ADR-0031 决策 8：claim 排给自己的 intent 是前端的定义性动作（ADR-0014），一个不能
        // claim 的前端不是前端，所以 frontend_open 不是可选声明而是隐含的。claim 只限本 id，
        // 由 api 层按主体校验。
        caps.push("frontend_open");
        caps.sort_unstable();
        caps.dedup();
        Some(caps)
    }

    pub fn default_frontend(&self) -> String {
        let selection = self.selection.lock().unwrap();
        if selection.is_healthy() && self.records.lock().unwrap().contains_key(&selection.id) { selection.id.clone() } else { BUILTIN_RECOVERY_FRONTEND.into() }
    }

    pub fn set_default(&self, id: &str) -> Result<(), String> {
        validate_frontend_id(id)?;
        let mut selection = self.selection.lock().unwrap();
        let records = self.records.lock().unwrap();
        if !records.contains_key(id) { return Err("前端未安装".into()); }
        selection.store(id)
    }

    pub fn can_remove(&self,id:&str) -> bool { self.selection.lock().unwrap().can_remove(id) }

    pub(crate) fn permits_package_mutation(&self) -> bool {
        self.directory_available.load(Ordering::Acquire) && self.catalog_authoritative.load(Ordering::Acquire)
    }

    /// 已发现的前端 id（含 builtin；身份对账由调用方过滤）。
    pub fn frontend_ids(&self) -> Vec<String> {
        self.records.lock().unwrap().keys().cloned().collect()
    }
}

/// staging 宽松校验结果（事务模块用）。
pub(crate) struct InspectedStagedFrontend {
    pub id: String,
    pub version: String,
    pub info: FrontendInfo,
}

pub(crate) fn inspect_staged_frontend(package: &Path) -> Result<InspectedStagedFrontend, String> {
    let record = load_package_at(package, None)?;
    Ok(InspectedStagedFrontend {
        id: record.info.id.clone(),
        version: record.info.version.clone(),
        info: record.info,
    })
}

/// Inspect the installed bundled frontend before automatic upgrade: historical `protocol_version`
/// values (`1..=max_protocol_version`) are eligible to be upgraded to the current bundled payload,
/// while damaged packages or future protocol generations (`> max_protocol_version`, e.g. after an
/// app rollback) are rejected so `refresh` leaves them untouched.
#[cfg(test)]
pub(crate) fn inspect_installed_for_upgrade(
    package: &Path,
    max_protocol_version: u32,
) -> Result<String, String> {
    inspect_installed_payload_for_upgrade(package, max_protocol_version, crate::recovery::LEGACY_PAYLOAD)
}

pub(crate) fn inspect_installed_payload_for_upgrade(
    package: &Path,
    max_protocol_version: u32,
    identity: crate::recovery::BundledIdentity,
) -> Result<String, String> {
    let metadata = fs::symlink_metadata(package)
        .map_err(|_| "frontend package cannot be inspected".to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("frontend package must be a directory".into());
    }
    validate_owned_entry(&metadata)?;
    let manifest_text = read_manifest(&package.join(MANIFEST_FILE))?;
    let manifest: FrontendManifest =
        toml::from_str(&manifest_text).map_err(|_| "frontend manifest is invalid".to_string())?;
    validate_manifest_with_protocol(
        &manifest,
        Some(identity.id),
        1..=max_protocol_version,
    )?;
    match (manifest.lifecycle, manifest.launch) {
        (FrontendLifecycle::OnDemand, Some(launch)) => {
            let _ = resolve_launch_path(package, &launch.path, launch.kind)?;
        }
        _ => return Err("installed bundled frontend launch shape is invalid".into()),
    }
    Ok(manifest.version)
}

/// Fixed bundled payloads must be complete embedded applications, never development manifests.
pub(crate) fn inspect_bundled_payload(
    package: &Path,
    identity: crate::recovery::BundledIdentity,
) -> Result<String, String> {
    let record = load_package_at(package, None)?;
    if record.info.id != identity.id
        || !matches!(&record.launch, LaunchTarget::MacosApplication(path)
            if *path == package.join(identity.app))
        || !FrontendCapability::ALL
            .iter()
            .all(|cap| record.capabilities.contains(cap))
    {
        return Err("恢复副本不是完整内置 Tauri 前端".into());
    }
    let contents = package.join(identity.app).join("Contents");
    // The staging copier already rejected links/special entries throughout the payload.
    for path in [&contents, &contents.join("MacOS")] {
        let metadata = fs::symlink_metadata(path).map_err(|_| "恢复应用目录不完整")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("恢复应用目录无效".into());
        }
        validate_owned_entry(&metadata)?;
    }
    let exe = contents.join("MacOS").join(identity.binary);
    let metadata = fs::symlink_metadata(&exe).map_err(|_| "恢复应用缺少可执行文件")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() < 32 {
        return Err("恢复应用可执行文件无效".into());
    }
    validate_owned_entry(&metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o100 == 0 {
            return Err("恢复应用缺少执行权限".into());
        }
    }
    let mut magic = [0; 4];
    fs::File::open(&exe)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
        .map_err(|_| "恢复可执行文件无法读取")?;
    // Mach-O 64-bit or universal binaries; scripts/text/truncated files are not recovery apps.
    if !matches!(
        magic,
        [0xcf, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbf, 0xba, 0xfe, 0xca]
    ) {
        return Err("恢复可执行文件不是 Mach-O".into());
    }
    // Generated from the verified executable at packaging time. Detect truncation/corruption
    // without inventing a Mach-O loader. This is integrity, not a signature/trust substitute.
    let expected = read_recovery_checksum(package)?;
    if executable_checksum(&exe)?.as_bytes() != expected {
        return Err("恢复应用内容与打包校验和不一致".into());
    }
    let plist = contents.join("Info.plist");
    let metadata = fs::symlink_metadata(&plist).map_err(|_| "恢复应用缺少 Info.plist")?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err("恢复应用 Info.plist 无效".into());
    }
    validate_owned_entry(&metadata)?;
    let bytes = fs::read(&plist).map_err(|_| "恢复应用 Info.plist 不可读取")?;
    let info = plist::Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|_| "恢复应用 Info.plist 解析失败")?;
    let info = info.as_dictionary().ok_or("恢复应用 Info.plist 不是字典")?;
    for (key, expected) in [
        ("CFBundleExecutable", identity.binary),
        ("CFBundleIdentifier", identity.id),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleShortVersionString", record.info.version.as_str()),
    ] {
        if info.get(key).and_then(plist::Value::as_string) != Some(expected) {
            return Err("恢复应用身份或版本与清单不一致".into());
        }
    }
    Ok(record.info.version)
}

/// `recovery.sha256` of a bundled payload: exactly 64 lowercase/uppercase hex digits and `\n`.
fn read_recovery_checksum(package: &Path) -> Result<Vec<u8>, String> {
    let checksum_path = package.join("recovery.sha256");
    let checksum_metadata =
        fs::symlink_metadata(&checksum_path).map_err(|_| "恢复副本缺少校验和")?;
    if !checksum_metadata.is_file()
        || checksum_metadata.file_type().is_symlink()
        || checksum_metadata.len() != 65
    {
        return Err("恢复副本校验和格式无效".into());
    }
    validate_owned_entry(&checksum_metadata)?;
    let expected = fs::read(&checksum_path).map_err(|_| "恢复副本校验和不可读取")?;
    if expected.len() != 65
        || expected[64] != b'\n'
        || !expected[..64].iter().all(u8::is_ascii_hexdigit)
    {
        return Err("恢复副本校验和格式无效".into());
    }
    Ok(expected.to_ascii_lowercase())
}

/// Lowercase hex SHA-256 plus `\n` of a bounded (512 MiB) executable.
fn executable_checksum(exe: &Path) -> Result<String, String> {
    use sha2::{Digest as _, Sha256};
    let mut binary = fs::File::open(exe)
        .map_err(|_| "恢复应用不可读取")?
        .take(512 * 1024 * 1024 + 1);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 32 * 1024];
    let mut bytes_read = 0_u64;
    loop {
        let n = binary.read(&mut buffer).map_err(|_| "恢复应用校验失败")?;
        if n == 0 {
            break;
        }
        bytes_read += n as u64;
        digest.update(&buffer[..n]);
    }
    if bytes_read > 512 * 1024 * 1024 {
        return Err("恢复应用超过大小上限".into());
    }
    Ok(format!("{:x}\n", digest.finalize()))
}

/// Comparison of an installed managed bundled frontend against the app bundle's packaged payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstalledFrontendMatch {
    /// Installed package is valid and matches the bundled payload's version and executable checksum.
    MatchesBundled,
    /// Installed package is valid and launchable, but its version differs from the bundled payload
    /// (e.g. a user-installed standalone Studio version or a rolled-back copy).
    DifferentVersion,
    /// Installed package is valid and has the same version as the bundled payload, but its
    /// executable bytes differ from `recovery.sha256` (e.g. app rebuilt/re-signed over an old one).
    Stale,
    /// Installed package is valid and launchable, but the bundled reference payload or checksum
    /// could not be read.
    Unreferenced,
}

/// Inspect an installed bundled frontend (`BUNDLED_FRONTEND_ID` or `STUDIO_FRONTEND_ID`) against
/// its packaged recovery payload.
pub fn inspect_installed_frontend(
    id: &str,
    installed: &Path,
    bundled: &Path,
) -> Result<InstalledFrontendMatch, String> {
    let identity = if id == crate::recovery::BUNDLED_FRONTEND_ID {
        crate::recovery::LEGACY_PAYLOAD
    } else if id == crate::recovery::STUDIO_FRONTEND_ID {
        crate::recovery::STUDIO_PAYLOAD
    } else {
        return Err("不支持的内置前端标识".into());
    };
    inspect_installed_payload(installed, bundled, identity)
}

/// ADR-0044: before the app bundle hosts its own `Contents/MacOS/<UI_BIN>` in place, the managed
/// installed bundled frontend must still be a loadable package (the daemon's catalog already
/// requires it) whose executable is intact. When the installed copy claims the SAME version as
/// the app's bundled payload, its executable bytes must match the bundled `recovery.sha256`;
/// same version with different bytes is damage and routes to native recovery (ADR-0039/0040).
/// Other versions (pending upgrade, rollback) are not byte-compared. An unreadable bundled
/// payload is not the installed package's fault and never blocks the window.
pub fn check_installed_bundled_frontend(installed: &Path, bundled: &Path) -> Result<(), String> {
    match inspect_installed_frontend(crate::recovery::BUNDLED_FRONTEND_ID, installed, bundled)? {
        InstalledFrontendMatch::MatchesBundled
        | InstalledFrontendMatch::DifferentVersion
        | InstalledFrontendMatch::Unreferenced => Ok(()),
        InstalledFrontendMatch::Stale => Err("已装内置前端与同版本打包校验和不一致".into()),
    }
}

/// The installed bundled frontend is a valid, launchable package of the SAME version as the
/// bundled payload, but its executable comes from a different build. A rebuilt or re-signed app
/// installed over the old one leaves exactly this behind (the version number did not move);
/// resyncing it from the bundled payload is an upgrade, not a repair of damage.
pub fn installed_bundled_frontend_is_stale(installed: &Path, bundled: &Path) -> bool {
    matches!(
        inspect_installed_frontend(crate::recovery::BUNDLED_FRONTEND_ID, installed, bundled),
        Ok(InstalledFrontendMatch::Stale)
    )
}

pub(crate) fn installed_payload_is_stale(
    installed: &Path,
    bundled: &Path,
    identity: crate::recovery::BundledIdentity,
) -> bool {
    matches!(
        inspect_installed_payload(installed, bundled, identity),
        Ok(InstalledFrontendMatch::Stale)
    )
}

fn inspect_installed_payload(
    installed: &Path,
    bundled: &Path,
    identity: crate::recovery::BundledIdentity,
) -> Result<InstalledFrontendMatch, String> {
    let record = load_package_at(installed, Some(identity.id))?;
    let LaunchTarget::MacosApplication(app) = &record.launch else {
        return Err("installed bundled frontend launch shape is invalid".into());
    };
    let exe = app.join("Contents/MacOS").join(identity.binary);
    let metadata = fs::symlink_metadata(&exe).map_err(|_| "已装内置前端缺少可执行文件")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() < 32 {
        return Err("已装内置前端可执行文件无效".into());
    }
    validate_owned_entry(&metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o100 == 0 {
            return Err("已装内置前端缺少执行权限".into());
        }
    }
    let Ok(reference) = load_package_at(bundled, Some(identity.id)) else {
        return Ok(InstalledFrontendMatch::Unreferenced);
    };
    if reference.info.version != record.info.version {
        return Ok(InstalledFrontendMatch::DifferentVersion);
    }
    let Ok(expected) = read_recovery_checksum(bundled) else {
        return Ok(InstalledFrontendMatch::Unreferenced);
    };
    if executable_checksum(&exe)?.as_bytes() != expected {
        return Ok(InstalledFrontendMatch::Stale);
    }
    Ok(InstalledFrontendMatch::MatchesBundled)
}

impl FrontendBroker {
    /// Explicit recovery alone may replace damaged selection state. Publish both snapshots only
    /// after the repaired catalog and newly written selection have been checked.
    pub(crate) fn reload_after_repair(&self, root: &Path, expected_id: &str) -> Result<(), String> {
        // Any failure after disk commit must not leave the stale in-memory selection permitting
        // removal of the actual persisted default. Successful validation replaces this state.
        self.selection.lock().unwrap().freeze();
        let catalog = FrontendCatalog::load(Some(root))?;
        let repaired = selection::Selection::load(expected_id, Some(root));
        if !repaired.has_state
            || repaired.id != expected_id
            || !repaired.is_healthy()
            || !catalog.records.contains_key(&repaired.id)
        {
            return Err("恢复后的默认前端状态无效".into());
        }
        let mut selection = self.selection.lock().unwrap();
        let mut records = self.records.lock().unwrap();
        *records = catalog.records;
        *selection = repaired;
        self.directory_available.store(true, Ordering::Release);
        self.catalog_authoritative.store(true, Ordering::Release);
        Ok(())
    }
}

/// Check the same non-hidden entry budget as the catalog, counting a new/replaced bundled package.
pub(crate) fn check_bundled_capacity(root: &Path, id: &str) -> Result<(), String> {
    let mut resulting = 1; // the replacement/new bundled frontend
    for entry in fs::read_dir(root).map_err(|_| "前端目录不可读取")? {
        let name = entry.map_err(|_| "前端条目不可读取")?.file_name();
        if name.to_string_lossy().starts_with('.') || name == id {
            continue;
        }
        resulting += 1;
        if resulting > MAX_FRONTENDS {
            return Err("恢复将超过前端目录数量上限，请先移除其他包".into());
        }
    }
    Ok(())
}

/// 发布位严格校验，返回 (id, version)。
pub(crate) fn inspect_frontend_at(package: &Path) -> Result<(String, String), String> {
    let name = package.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let record = load_package_at(package, Some(name))?;
    Ok((record.info.id.clone(), record.info.version.clone()))
}

pub(crate) fn validate_config(config: &FrontendConfig) -> Result<(), String> {
    validate_frontend_id(&config.default)
}

fn validate_frontend_id(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    if value.is_empty()
        || value.len() > MAX_FRONTEND_ID_BYTES
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("frontend id 无效".into());
    }
    Ok(())
}

fn validate_intent(intent: &OpenIntent) -> Result<(), String> {
    let valid_field = |value: &str| {
        !value.is_empty()
            && value.len() <= MAX_INTENT_FIELD_BYTES
            && !value.chars().any(char::is_control)
    };
    match intent {
        OpenIntent::Inbox {
            account: Some(account),
        } => valid_field(account),
        OpenIntent::Inbox { account: None }
        | OpenIntent::Compose
        | OpenIntent::Settings
        | OpenIntent::Background => true,
        OpenIntent::Message {
            account,
            uid,
            folder,
        } => {
            valid_field(account)
                && valid_field(uid)
                && folder.as_ref().is_none_or(|v| {
                    !v.is_empty() && v.len() <= 512 && !v.chars().any(char::is_control)
                })
        }
        OpenIntent::Unknown => false,
    }
    .then_some(())
    .ok_or_else(|| "open intent 字段无效".into())
}

/// 路径式校验；`expected_id` 为 None 时跳过「id == 目录名」（staging 场景）。
fn load_package_at(package: &Path, expected_id: Option<&str>) -> Result<FrontendRecord, String> {
    let metadata = fs::symlink_metadata(package)
        .map_err(|_| "frontend package cannot be inspected".to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("frontend package must be a directory".into());
    }
    validate_owned_entry(&metadata)?;

    let manifest_path = package.join(MANIFEST_FILE);
    let manifest_text = read_manifest(&manifest_path)?;
    let manifest: FrontendManifest =
        toml::from_str(&manifest_text).map_err(|_| "frontend manifest is invalid".to_string())?;
    let text = validate_manifest(&manifest, expected_id)?;
    let launch = match (manifest.lifecycle, manifest.launch) {
        (FrontendLifecycle::Independent, None) => LaunchTarget::Independent,
        (FrontendLifecycle::OnDemand, Some(launch)) => {
            let path = resolve_launch_path(package, &launch.path, launch.kind)?;
            match launch.kind {
                LaunchKind::MacosApplication => LaunchTarget::MacosApplication(path),
            }
        }
        (FrontendLifecycle::Independent, Some(_)) => {
            return Err("independent frontend must not declare launch".into())
        }
        (FrontendLifecycle::OnDemand, None) => {
            return Err("on_demand frontend requires launch".into())
        }
    };
    let capabilities = manifest.capabilities.into_iter().collect::<HashSet<_>>();
    let mut capability_names = capabilities
        .iter()
        .map(|capability| capability.as_str().to_string())
        .collect::<Vec<_>>();
    capability_names.sort();
    let permissions = manifest.permissions.into_iter().collect::<HashSet<_>>();
    let mut permission_names = permissions
        .iter()
        .map(|permission| permission.as_str().to_string())
        .collect::<Vec<_>>();
    permission_names.sort();
    Ok(FrontendRecord {
        info: FrontendInfo {
            id: manifest.id,
            name: manifest.name,
            description: text.description,
            l10n: text.l10n,
            version: manifest.version,
            lifecycle: manifest.lifecycle.as_str().into(),
            capabilities: capability_names,
            permissions: permission_names,
            builtin: false,
            has_identity: false,
            denied_calls: 0,
        },
        capabilities,
        permissions,
        launch,
    })
}

fn read_manifest(path: &Path) -> Result<String, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "frontend manifest is missing".to_string())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err("frontend manifest type or size is invalid".into());
    }
    validate_owned_entry(&metadata)?;
    let file =
        fs::File::open(path).map_err(|_| "frontend manifest cannot be opened".to_string())?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "frontend manifest cannot be read".to_string())?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("frontend manifest is too large".into());
    }
    String::from_utf8(bytes).map_err(|_| "frontend manifest is not UTF-8".into())
}

fn validate_manifest(
    manifest: &FrontendManifest,
    package_id: Option<&str>,
) -> Result<crate::manifest_l10n::PackageText, String> {
    validate_manifest_with_protocol(
        manifest,
        package_id,
        API_PROTOCOL_VERSION..=API_PROTOCOL_VERSION,
    )
}

/// 校验通过时返回校验后的 `description` / `l10n`（ADR-0048 决策 5），供目录与预览透传。
fn validate_manifest_with_protocol(
    manifest: &FrontendManifest,
    package_id: Option<&str>,
    allowed_protocol: std::ops::RangeInclusive<u32>,
) -> Result<crate::manifest_l10n::PackageText, String> {
    if !matches!(
        manifest.schema_version,
        MANIFEST_SCHEMA_VERSION_V1
            | MANIFEST_SCHEMA_VERSION_PERMISSIONS
            | MANIFEST_SCHEMA_VERSION_L10N
    ) || !allowed_protocol.contains(&manifest.protocol_version)
    {
        return Err("frontend manifest version is unsupported".into());
    }
    validate_frontend_id(&manifest.id)?;
    if is_builtin_frontend(&manifest.id) { return Err("内置恢复标识不可由包声明".into()); }
    if package_id.is_some_and(|package_id| manifest.id != package_id) {
        return Err("frontend manifest id must match its directory".into());
    }
    if !valid_text(&manifest.name, MAX_NAME_BYTES)
        || manifest.version.is_empty()
        || manifest.version.len() > MAX_VERSION_BYTES
        || !manifest
            .version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
    {
        return Err("frontend name or version is invalid".into());
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
        return Err("frontend capabilities are invalid".into());
    }
    // v1 包写 permissions 直接拒：一个需要写权限才能工作的前端，不该以静默零权限的形态装上
    // 去（ADR-0030 决策 3）。
    if manifest.schema_version == MANIFEST_SCHEMA_VERSION_V1 && !manifest.permissions.is_empty() {
        return Err("frontend permissions require manifest schema_version 2".into());
    }
    if manifest.permissions.len() > MAX_PERMISSIONS
        || manifest
            .permissions
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len()
            != manifest.permissions.len()
    {
        return Err("frontend permissions are invalid".into());
    }
    // 与 permissions 同理：v1/v2 包写了这两个键直接拒，旧 daemon 本来也会因未知键拒绝它。
    if manifest.schema_version < MANIFEST_SCHEMA_VERSION_L10N
        && (manifest.description.is_some() || !manifest.l10n.is_empty())
    {
        return Err("frontend description and l10n require manifest schema_version 3".into());
    }
    crate::manifest_l10n::validate(manifest.description.as_deref(), &manifest.l10n, MAX_NAME_BYTES)
        .map_err(|reason| format!("frontend {reason}"))
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn resolve_launch_path(root: &Path, relative: &Path, kind: LaunchKind) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("frontend launch path must remain inside its package".into());
    }
    let mut current = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|_| "frontend launch path is missing".to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("frontend launch path must not contain symlinks".into());
        }
        validate_owned_entry(&metadata)?;
        let is_final = index + 1 == components.len();
        if !is_final && !metadata.is_dir() {
            return Err("frontend launch parent must be a directory".into());
        }
        if is_final {
            match kind {
                LaunchKind::MacosApplication
                    if metadata.is_dir()
                        && current.extension().and_then(|value| value.to_str()) == Some("app") => {}
                _ => return Err("frontend launch target type is invalid".into()),
            }
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
            "frontend package entries must be current-user owned and not writable by others".into(),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_owned_entry(_metadata: &fs::Metadata) -> Result<(), String> {
    Ok(())
}

/// Frontend spine plugin. It is independent from any concrete UI process.
pub struct FrontendPlugin;

impl Plugin for FrontendPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<FrontendConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let config = ctx.get::<FrontendConfig>().expect("inject 就绪");
        let root = ctx.get::<FrontendInstallRoot>();
        let lease = if ctx.get::<FrontendDirectoryUnavailable>().is_some() {
            Some(Err("前端目录准备失败".into()))
        } else { root.as_ref().map(|root| FrontendDirectoryLease::acquire(&root.0)) };
        let (lease, broker) = match lease {
            Some(Err(_)) => {
                eprintln!("[maild/frontend] 前端目录未取得所有权，保留原生修复入口");
                (None, FrontendBroker::without_directory(&config)?)
            }
            lease => {
                let lease = lease.transpose()?;
                // First-run preinstall (ADR-0039) runs here, under the directory lease and before
                // the first scan, so login-item, Preview and upgraded Agents all get it. It is
                // no-clobber and marker-guarded: a user-removed package is never reinstalled.
                if let (Some(_), Some(root), Some(source)) =
                    (&lease, &root, ctx.get::<crate::recovery::BundledFrontend>())
                {
                    match source.preinstall(&root.0) {
                        Ok(crate::recovery::PreinstallOutcome::Installed) => {
                            eprintln!("[maild/frontend] 已预装内置前端");
                        }
                        Ok(_) => {}
                        Err(_) => eprintln!("[maild/frontend] 内置前端预装未完成，可从原生修复入口恢复"),
                    }
                    // App upgrades carry a newer bundled UI; older installed copies follow it.
                    match source.refresh(&root.0) {
                        Ok(crate::recovery::RefreshOutcome::Upgraded { from, to }) => {
                            eprintln!("[maild/frontend] 内置前端已从 {from} 升级到 {to}，旧包已备份");
                        }
                        Ok(crate::recovery::RefreshOutcome::Resynced { version }) => {
                            eprintln!("[maild/frontend] 内置前端 {version} 已同步为随包构建，旧包已备份");
                        }
                        Ok(_) => {}
                        Err(_) => eprintln!("[maild/frontend] 内置前端升级未完成，继续使用已安装版本"),
                    }
                }
                // Studio is bundled alongside the legacy UI and becomes the native repair target
                // when present; preinstall and refresh still respect removal and never write defaults.
                if let (Some(_), Some(root), Some(source)) =
                    (&lease, &root, ctx.get::<crate::recovery::BundledFrontend>())
                {
                    match source.studio() {
                        Ok(Some(studio)) => {
                            if studio.preinstall(&root.0).is_err() {
                                eprintln!("[maild/frontend] Studio 预装未完成，保留现有前端");
                            }
                            if studio.refresh(&root.0).is_err() {
                                eprintln!("[maild/frontend] Studio 升级未完成，保留现有前端");
                            }
                        }
                        Ok(None) => {}
                        Err(_) => eprintln!("[maild/frontend] Studio 随包副本不可用"),
                    }
                }
                (lease, FrontendBroker::with_install_root(
                    &config, root.as_ref().map(|root| root.0.as_path()))?)
            }
        };
        let broker = Arc::new(broker);
        // Owned by this plugin fiber: teardown revokes it even if unrelated contexts linger.
        // In-flight recovery jobs retain their Arc until publication/refresh have finished.
        let owns_directory = lease.is_some();
        if let Some(lease) = lease { ctx.provide(Arc::new(lease)); }
        ctx.provide::<FrontendBroker>(broker.clone());
        // ADR-0045：入队即广播 `FrontendIntentQueued`，API 层只推给该前端自己的认证长连接，
        // 隐藏在菜单栏的窗口据此回到前台。卸载时撤回，断开 broker → ctx 的引用。
        {
            let emitter = ctx.clone();
            broker.set_intent_waker(Some(Arc::new(move |frontend: &str| {
                emitter.emit::<crate::events::FrontendIntentQueued>(frontend.to_owned());
            })));
            let owner = Arc::downgrade(&broker);
            ctx.effect_sync(move || {
                if let Some(broker) = owner.upgrade() {
                    broker.set_intent_waker(None);
                }
            });
        }
        // 身份缝（ADR-0017）：为第三方前端包对账条目；builtin 是进程内可信组件，
        // 不参与逐插件身份。
        if let Some(seam) = ctx.get::<IdentitySeam>().filter(|_| owns_directory && broker.catalog_authoritative.load(Ordering::Acquire)) {
            let ids: Vec<String> = broker
                .frontend_ids()
                .into_iter()
                .filter(|id| !is_builtin_frontend(id))
                .collect();
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            let _ = seam.reconcile(PluginKind::Frontend, &refs);
        }
        if let Some(notifier) = ctx.get::<Notifier>() {
            notifier.attach_frontend(&broker);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn route_translation_maps_frontend_routes_to_api_capabilities() {
        let root = install_root();
        write_independent(
            root.path(),
            "org.example.all",
            "\"inbox\", \"message\", \"compose\", \"settings\", \"background\"",
        );
        let broker = broker_over(root.path());
        // inbox/message→messages_read、compose→messages_send、
        // settings→settings_read+settings_write、background→无。
        assert_eq!(
            broker.api_capabilities_of("org.example.all").unwrap(),
            vec![
                "frontend_open",
                "messages_read",
                "messages_send",
                "settings_read",
                "settings_write"
            ]
        );
        // 原生恢复入口只路由 background，不授权任何邮件 API。
        assert_eq!(
            broker.api_capabilities_of(BUILTIN_RECOVERY_FRONTEND).unwrap(),
            vec!["frontend_open"]
        );
        // 未在册的 id → None（fail-closed 空表）；旧 egui id 不再是目录条目。
        assert_eq!(broker.api_capabilities_of("org.example.missing"), None);
        assert_eq!(broker.api_capabilities_of(LEGACY_EGUI_FRONTEND), None);
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct MockLauncher(AtomicUsize);

    impl FrontendLauncher for MockLauncher {
        fn activate(&self, _frontend: &FrontendRecord, _is_default: bool) -> Result<bool, &'static str> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(true)
        }
    }

    #[cfg(unix)]
    fn broker_with(root: &Path, config: &FrontendConfig, launcher: Arc<MockLauncher>) -> FrontendBroker {
        FrontendBroker::from_catalog(config, FrontendCatalog::load(Some(root)).unwrap(), launcher)
    }

    #[cfg(unix)]
    #[test]
    fn broker_routes_bounded_one_shot_intent_to_default_frontend() {
        let root = install_root();
        write_independent(root.path(), DEFAULT_FRONTEND, "\"inbox\", \"message\"");
        let launcher = Arc::new(MockLauncher(AtomicUsize::new(0)));
        let broker = broker_with(root.path(), &FrontendConfig::default(), launcher.clone());
        assert_eq!(broker.default_frontend(), DEFAULT_FRONTEND);
        let intent = OpenIntent::Message {
            folder: None,
            account: "main".into(),
            uid: "42".into(),
        };
        assert!(broker.open(intent.clone()).unwrap());
        assert_eq!(launcher.0.load(Ordering::Relaxed), 1);
        assert_eq!(broker.claim(DEFAULT_FRONTEND).unwrap(), Some(intent));
        assert_eq!(broker.claim(DEFAULT_FRONTEND).unwrap(), None);
    }

    /// ADR-0045：每次入队在队列锁外唤醒目标前端；回调里回头 claim 不死锁；无效意图不唤醒；
    /// 撤回后不再唤醒。
    #[cfg(unix)]
    #[test]
    fn enqueue_wakes_the_target_frontend_outside_the_queue_lock() {
        let root = install_root();
        write_independent(root.path(), DEFAULT_FRONTEND, "\"inbox\", \"message\"");
        let launcher = Arc::new(MockLauncher(AtomicUsize::new(0)));
        let broker = Arc::new(broker_with(root.path(), &FrontendConfig::default(), launcher));
        let woken = Arc::new(Mutex::new(Vec::<(String, Option<OpenIntent>)>::new()));
        {
            let woken = woken.clone();
            let weak = Arc::downgrade(&broker);
            broker.set_intent_waker(Some(Arc::new(move |frontend: &str| {
                let claimed = weak.upgrade().and_then(|broker| broker.claim(frontend).ok().flatten());
                woken.lock().unwrap().push((frontend.to_owned(), claimed));
            })));
        }
        assert!(broker
            .open(OpenIntent::Message { folder: None, account: "main\nx".into(), uid: "1".into() })
            .is_err());
        assert!(woken.lock().unwrap().is_empty(), "无效意图不入队也不唤醒");
        assert!(broker.open(OpenIntent::Inbox { account: None }).unwrap());
        assert_eq!(
            woken.lock().unwrap().as_slice(),
            &[(DEFAULT_FRONTEND.to_owned(), Some(OpenIntent::Inbox { account: None }))]
        );
        broker.set_intent_waker(None);
        assert!(broker.open(OpenIntent::Inbox { account: None }).unwrap());
        assert_eq!(woken.lock().unwrap().len(), 1, "撤回后不再唤醒");
        assert_eq!(broker.claim(DEFAULT_FRONTEND).unwrap(), Some(OpenIntent::Inbox { account: None }));
    }

    /// ADR-0039：旧 egui 默认在读取时迁移为 Tauri 默认，不改写用户文件；显式请求旧 id
    /// 走原生恢复别名，只携带 background。
    #[cfg(unix)]
    #[test]
    fn legacy_egui_default_migrates_and_explicit_requests_reach_recovery() {
        let root = install_root();
        write_independent(root.path(), DEFAULT_FRONTEND, "\"inbox\", \"message\"");
        let legacy = FrontendConfig { default: LEGACY_EGUI_FRONTEND.into() };
        assert!(validate_config(&legacy).is_ok());
        let broker = FrontendBroker::with_install_root(&legacy, Some(root.path())).unwrap();
        assert_eq!(broker.default_frontend(), DEFAULT_FRONTEND);
        assert_eq!(broker.frontends(None).default, DEFAULT_FRONTEND);

        let state = root.path().join(".default.json");
        let legacy_state = b"{\"version\":1,\"default\":\"builtin-egui\"}";
        fs::write(&state, legacy_state).unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let launcher = Arc::new(MockLauncher(AtomicUsize::new(0)));
        let broker = FrontendBroker::with_install_root(&FrontendConfig { default: "org.example.other".into() }, Some(root.path())).unwrap();
        assert_eq!(broker.default_frontend(), DEFAULT_FRONTEND);
        assert_eq!(fs::read(&state).unwrap(), legacy_state, "迁移只发生在读取时");

        let broker = broker_with(root.path(), &legacy, launcher.clone());
        assert!(broker
            .open_for(Some(LEGACY_EGUI_FRONTEND), OpenIntent::Inbox { account: None })
            .unwrap());
        assert_eq!(launcher.0.load(Ordering::Relaxed), 1);
        assert_eq!(broker.claim(BUILTIN_RECOVERY_FRONTEND).unwrap(), Some(OpenIntent::Background));
        assert!(broker.claim(LEGACY_EGUI_FRONTEND).is_err());
        assert!(broker.set_default(LEGACY_EGUI_FRONTEND).is_err());
    }

    #[test]
    fn invalid_fields_and_frontend_ids_fail_closed() {
        let broker = FrontendBroker::with_launcher(
            &FrontendConfig::default(),
            Arc::new(MockLauncher(AtomicUsize::new(0))),
        );
        assert!(broker
            .open(OpenIntent::Message {
                folder: None,
                account: "main\nother".into(),
                uid: "1".into(),
            })
            .is_err());
        assert!(broker.claim("../evil").is_err());
        assert!(validate_config(&FrontendConfig {
            default: "valid-but-missing".into(),
        })
        .is_ok());
        let missing = FrontendBroker::new(&FrontendConfig { default: "valid-but-missing".into() }).unwrap();
        assert_eq!(missing.default_frontend(), BUILTIN_RECOVERY_FRONTEND);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_listing_names_itself_in_every_kernel_language() {
        let info = FrontendRecord::recovery().info;
        assert_eq!(info.name, "Featherpost 修复与后台管理");
        assert_eq!(info.l10n["en"].name.as_deref(), Some("Featherpost Repair & Background"));
        assert_eq!(info.l10n["zh-Hans"].name.as_deref(), Some("Featherpost 修复与后台管理"));
    }

    fn install_root() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[cfg(unix)]
    fn write_independent(root: &Path, id: &str, capabilities: &str) {
        write_manifest(root, id, 1, capabilities, None);
    }

    /// `permissions` 为 `Some` 时写出该行，便于覆盖 v1/v2 与坏词的组合。
    #[cfg(unix)]
    fn write_manifest(
        root: &Path,
        id: &str,
        schema_version: u32,
        capabilities: &str,
        permissions: Option<&str>,
    ) {
        let permissions = permissions
            .map(|value| format!("permissions = [{value}]\n"))
            .unwrap_or_default();
        write_manifest_with(root, id, schema_version, capabilities, &permissions);
    }

    /// 同 [`write_manifest`]，`extra` 原样追加在 manifest 末尾（顶层键或子表）。
    #[cfg(unix)]
    fn write_manifest_with(
        root: &Path,
        id: &str,
        schema_version: u32,
        capabilities: &str,
        extra: &str,
    ) {
        use std::os::unix::fs::PermissionsExt as _;
        let package = root.join(id);
        fs::create_dir(&package).unwrap();
        fs::set_permissions(&package, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            package.join(MANIFEST_FILE),
            format!(
                "schema_version = {schema_version}\nprotocol_version = 1\nid = \"{id}\"\nname = \"Test frontend\"\nversion = \"1.0.0\"\nlifecycle = \"independent\"\ncapabilities = [{capabilities}]\n{extra}"
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
    fn broker_over(root: &Path) -> FrontendBroker {
        FrontendBroker::with_install_root(
            &FrontendConfig {
                default: DEFAULT_FRONTEND.into(),
            },
            Some(root),
        )
        .unwrap()
    }

    /// ADR-0030 决策 2：授权是路由之外的一组词，两者并集才是有效授权。
    #[cfg(unix)]
    #[test]
    fn declared_permissions_join_route_translation() {
        let root = install_root();
        write_manifest(
            root.path(),
            "org.example.full",
            2,
            "\"inbox\", \"message\", \"compose\"",
            Some("\"mailbox_write\", \"accounts_manage\""),
        );
        let broker = broker_over(root.path());
        assert_eq!(
            broker.api_capabilities_of("org.example.full").unwrap(),
            vec![
                "accounts_manage",
                "frontend_open",
                "mailbox_write",
                "messages_read",
                "messages_send"
            ]
        );
        let listed = broker.frontends(None);
        let entry = listed
            .frontends
            .iter()
            .find(|entry| entry.id == "org.example.full")
            .unwrap();
        // 声明要可审计：listFrontends 把它原样交给用户（决策 1）。
        assert_eq!(entry.permissions, vec!["accounts_manage", "mailbox_write"]);
        // 路由语义不变：声明写权限不会凭空多出一个 route。
        assert_eq!(entry.capabilities, vec!["compose", "inbox", "message"]);
    }

    /// 不声明就没有——已安装包的有效权限逐字节不变（决策 2 兼容性）。
    #[cfg(unix)]
    #[test]
    fn routes_alone_never_grant_write_or_management() {
        let root = install_root();
        write_independent(root.path(), "org.example.reader", "\"inbox\", \"message\", \"settings\"");
        let broker = broker_over(root.path());
        let caps = broker.api_capabilities_of("org.example.reader").unwrap();
        assert_eq!(
            caps,
            vec!["frontend_open", "messages_read", "settings_read", "settings_write"]
        );
        for word in [
            "mailbox_write",
            "accounts_manage",
            "addresses_manage",
            "plugins_manage",
            "notifications_read",
            "drafts_read",
            "drafts_write",
        ] {
            assert!(!caps.contains(&word), "{word} 不应来自路由声明");
        }
        // ADR-0034 决策 7：`compose` 路由只说明「写信这个 intent 交给我」，
        // 它不隐含存草稿的授权——想存就自己写出来。
        write_independent(root.path(), "org.example.composer", "\"compose\"");
        let broker = broker_over(root.path());
        let composer = broker.api_capabilities_of("org.example.composer").unwrap();
        assert!(composer.contains(&"messages_send"));
        assert!(!composer.contains(&"drafts_write"));
        // 原生恢复入口同样是空授权表（决策 9）。
        let builtin = broker.frontends(None);
        let builtin = builtin
            .frontends
            .iter()
            .find(|entry| entry.id == BUILTIN_RECOVERY_FRONTEND)
            .unwrap();
        assert!(builtin.permissions.is_empty());
    }

    /// 决策 3：v1 包写 permissions 拒装，不降权静默装上。未知词同样整包拒绝。
    #[cfg(unix)]
    #[test]
    fn permissions_require_schema_two_and_a_known_vocabulary() {
        let root = install_root();
        write_manifest(root.path(), "org.example.v1perm", 1, "\"inbox\"", Some("\"mailbox_write\""));
        write_manifest(root.path(), "org.example.unknown", 2, "\"inbox\"", Some("\"root\""));
        write_manifest(root.path(), "org.example.dupe", 2, "\"inbox\"", Some("\"mailbox_write\", \"mailbox_write\""));
        write_manifest(root.path(), "org.example.ok", 2, "\"inbox\"", Some("\"mailbox_write\""));
        let broker = broker_over(root.path());
        for rejected in [
            "org.example.v1perm",
            "org.example.unknown",
            "org.example.dupe",
        ] {
            assert_eq!(
                broker.api_capabilities_of(rejected),
                None,
                "{rejected} 应整包拒绝"
            );
        }
        // 坏包不连累好包：目录里还有一个 v2 包正常装上。
        assert_eq!(
            broker.api_capabilities_of("org.example.ok").unwrap(),
            vec!["frontend_open", "mailbox_write", "messages_read"]
        );
    }

    /// ADR-0048 决策 5：`description` 与 `[l10n.<tag>]` 只在 v3 起允许，校验后原样进
    /// `listFrontends`；v1/v2 包照旧，JSON 里不出现这两个键。
    #[cfg(unix)]
    #[test]
    fn l10n_requires_schema_three_and_passes_through_listing() {
        let root = install_root();
        let l10n = "description = \"A mail client\"\npermissions = [\"mailbox_write\"]\n\
                    [l10n.EN]\ndescription = \"Mail client\"\n\
                    [l10n.\"zh-hans\"]\nname = \"测试前端\"\ndescription = \"邮件客户端\"\n";
        write_manifest_with(root.path(), "org.example.l10n", 3, "\"inbox\"", l10n);
        write_manifest_with(root.path(), "org.example.v2l10n", 2, "\"inbox\"", "[l10n.en]\nname = \"n\"\n");
        write_manifest_with(root.path(), "org.example.v2desc", 2, "\"inbox\"", "description = \"d\"\n");
        write_manifest_with(root.path(), "org.example.badtag", 3, "\"inbox\"", "[l10n.\"no tag\"]\nname = \"n\"\n");
        write_manifest_with(root.path(), "org.example.badkey", 3, "\"inbox\"", "[l10n.en]\nsummary = \"n\"\n");
        write_manifest(root.path(), "org.example.v2", 2, "\"inbox\"", None);
        let broker = broker_over(root.path());
        for rejected in [
            "org.example.v2l10n",
            "org.example.v2desc",
            "org.example.badtag",
            "org.example.badkey",
        ] {
            assert_eq!(broker.api_capabilities_of(rejected), None, "{rejected} 应整包拒绝");
        }
        let listed = serde_json::to_value(broker.frontends(None)).unwrap();
        let entry = |id: &str| {
            listed["frontends"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == id)
                .unwrap()
                .clone()
        };
        let localized = entry("org.example.l10n");
        assert_eq!(localized["name"], "Test frontend");
        assert_eq!(localized["description"], "A mail client");
        assert_eq!(
            localized["l10n"],
            serde_json::json!({
                "en": { "description": "Mail client" },
                "zh-Hans": { "name": "测试前端", "description": "邮件客户端" }
            })
        );
        // v3 仍可声明 permissions：新版本是 v2 的超集。
        assert_eq!(localized["permissions"], serde_json::json!(["mailbox_write"]));
        let plain = entry("org.example.v2");
        assert!(plain.get("l10n").is_none() && plain.get("description").is_none(), "{plain}");
        // 内置修复入口没有 manifest，名字按内核语言目录给出各语言版本。
        let recovery = entry(BUILTIN_RECOVERY_FRONTEND);
        assert!(recovery.get("description").is_none(), "{recovery}");
        assert_eq!(recovery["l10n"]["en"]["name"], "Featherpost Repair & Background");
    }

    /// ADR-0031 决策 8：notifications_read 是可声明的词，走 permissions 而不是塞进 settings 路由。
    #[cfg(unix)]
    #[test]
    fn notifications_read_is_declared_not_implied_by_settings_route() {
        let root = install_root();
        write_manifest(root.path(), "org.example.settingsonly", 2, "\"settings\"", None);
        write_manifest(root.path(), "org.example.notify", 2, "\"settings\"", Some("\"notifications_read\""));
        let broker = broker_over(root.path());
        assert!(!broker
            .api_capabilities_of("org.example.settingsonly")
            .unwrap()
            .contains(&"notifications_read"));
        assert!(broker
            .api_capabilities_of("org.example.notify")
            .unwrap()
            .contains(&"notifications_read"));
    }

    #[cfg(unix)]
    #[test]
    fn inspected_independent_frontend_can_be_default_and_is_enumerated() {
        let root = install_root();
        write_independent(root.path(), "org.example.reader", "\"inbox\", \"message\"");
        let broker = FrontendBroker::with_install_root(
            &FrontendConfig {
                default: "org.example.reader".into(),
            },
            Some(root.path()),
        )
        .unwrap();
        let result = broker.frontends(None);
        assert_eq!(result.default, "org.example.reader");
        assert_eq!(result.frontends.len(), 2);
        assert!(!broker.open(OpenIntent::Inbox { account: None }).unwrap());
        assert!(matches!(
            broker.claim("org.example.reader").unwrap(),
            Some(OpenIntent::Inbox { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_default_route_uses_builtin_recovery_and_bad_packages_are_ignored() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = install_root();
        write_independent(root.path(), "org.example.reader", "\"inbox\"");
        let bad = root.path().join("org.example.bad");
        fs::create_dir(&bad).unwrap();
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(bad.join(MANIFEST_FILE), "not = [valid").unwrap();
        fs::set_permissions(bad.join(MANIFEST_FILE), fs::Permissions::from_mode(0o600)).unwrap();
        let broker = FrontendBroker::with_install_root(
            &FrontendConfig {
                default: "org.example.reader".into(),
            },
            Some(root.path()),
        )
        .unwrap();
        let intent = OpenIntent::Settings;
        assert!(!broker.open(intent.clone()).unwrap());
        assert_eq!(broker.claim(BUILTIN_RECOVERY_FRONTEND).unwrap(), Some(OpenIntent::Background));
        assert!(broker.claim(LEGACY_EGUI_FRONTEND).is_err());
        assert!(broker.claim("org.example.bad").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn launch_paths_cannot_escape_the_package() {
        let root = install_root();
        let package = root.path().join("org.example.escape");
        fs::create_dir(&package).unwrap();
        assert!(resolve_launch_path(
            &package,
            Path::new("../outside"),
            LaunchKind::MacosApplication
        )
        .is_err());
    }

    #[test]
    fn only_the_default_bundled_frontend_is_hosted_by_the_top_level_app() {
        let app = Path::new("/Applications").join(maild_brand::APP_BUNDLE_NAME);
        for id in [
            crate::recovery::BUNDLED_FRONTEND_ID,
            crate::recovery::STUDIO_FRONTEND_ID,
        ] {
            assert_eq!(single_dock_host(id, true, || Some(app.clone())), Some(app.clone()));
            // Ineligible session/layout keeps the installed app.
            assert_eq!(single_dock_host(id, true, || None), None);
            // Non-default explicit target never redirects to Featherpost.app (which opens the default).
            assert_eq!(
                single_dock_host(id, false, || panic!("non-default target is not redirected")),
                None
            );
        }
        // A third-party frontend never even evaluates the in-place host.
        assert_eq!(
            single_dock_host("org.example.reader", true, || panic!(
                "third party is not hosted"
            )),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn broker_marks_default_vs_explicit_non_default_when_activating() {
        struct CaptureLauncher(Mutex<Vec<(String, bool)>>);
        impl FrontendLauncher for CaptureLauncher {
            fn activate(
                &self,
                frontend: &FrontendRecord,
                is_default: bool,
            ) -> Result<bool, &'static str> {
                self.0
                    .lock()
                    .unwrap()
                    .push((frontend.info.id.clone(), is_default));
                Ok(true)
            }
        }
        let root = install_root();
        write_independent(root.path(), crate::recovery::STUDIO_FRONTEND_ID, "\"inbox\"");
        write_independent(root.path(), crate::recovery::BUNDLED_FRONTEND_ID, "\"inbox\"");
        let launcher = Arc::new(CaptureLauncher(Mutex::new(Vec::new())));
        let broker = FrontendBroker::from_catalog(
            &FrontendConfig {
                default: crate::recovery::STUDIO_FRONTEND_ID.into(),
            },
            FrontendCatalog::load(Some(root.path())).unwrap(),
            launcher.clone(),
        );
        broker.open(OpenIntent::Inbox { account: None }).unwrap();
        broker
            .open_for(
                Some(crate::recovery::STUDIO_FRONTEND_ID),
                OpenIntent::Inbox { account: None },
            )
            .unwrap();
        broker
            .open_for(
                Some(crate::recovery::BUNDLED_FRONTEND_ID),
                OpenIntent::Inbox { account: None },
            )
            .unwrap();
        assert_eq!(
            *launcher.0.lock().unwrap(),
            vec![
                (crate::recovery::STUDIO_FRONTEND_ID.into(), true),
                (crate::recovery::STUDIO_FRONTEND_ID.into(), true),
                (crate::recovery::BUNDLED_FRONTEND_ID.into(), false),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn studio_installed_frontend_inspection_distinguishes_match_stale_different_version_and_damage() {
        let bundle = tempfile::tempdir().unwrap();
        let (_legacy, studio) = crate::recovery::tests::studio_source_fixture(bundle.path());
        let bundled = bundle
            .path()
            .join(maild_brand::APP_BUNDLE_NAME)
            .join("Contents/Resources/frontends")
            .join(crate::recovery::STUDIO_FRONTEND_ID);
        let root = tempfile::tempdir().unwrap();
        studio.preinstall(root.path()).unwrap();
        let installed = root.path().join(crate::recovery::STUDIO_FRONTEND_ID);
        assert_eq!(
            inspect_installed_frontend(crate::recovery::STUDIO_FRONTEND_ID, &installed, &bundled)
                .unwrap(),
            InstalledFrontendMatch::MatchesBundled
        );

        // Same version, different bytes -> Stale.
        let exe = installed
            .join(crate::recovery::STUDIO_PAYLOAD.app)
            .join("Contents/MacOS")
            .join(crate::recovery::STUDIO_PAYLOAD.binary);
        let mut bytes = fs::read(&exe).unwrap();
        bytes[100] ^= 0xff;
        fs::write(&exe, &bytes).unwrap();
        assert_eq!(
            inspect_installed_frontend(crate::recovery::STUDIO_FRONTEND_ID, &installed, &bundled)
                .unwrap(),
            InstalledFrontendMatch::Stale
        );

        // Different standalone Studio version -> DifferentVersion (launcher falls back to bundle).
        let manifest = installed.join(MANIFEST_FILE);
        let text = fs::read_to_string(&manifest).unwrap();
        fs::write(&manifest, text.replace("version = \"0.3.3\"", "version = \"0.4.0\"")).unwrap();
        assert_eq!(
            inspect_installed_frontend(crate::recovery::STUDIO_FRONTEND_ID, &installed, &bundled)
                .unwrap(),
            InstalledFrontendMatch::DifferentVersion
        );

        // Missing reference payload -> Unreferenced.
        assert_eq!(
            inspect_installed_frontend(
                crate::recovery::STUDIO_FRONTEND_ID,
                &installed,
                &bundle.path().join("missing-studio")
            )
            .unwrap(),
            InstalledFrontendMatch::Unreferenced
        );

        // Missing executable -> Err (damaged).
        fs::remove_file(&exe).unwrap();
        assert!(
            inspect_installed_frontend(crate::recovery::STUDIO_FRONTEND_ID, &installed, &bundled)
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn installed_bundled_frontend_must_match_same_version_bundle_bytes() {
        let bundle = tempfile::tempdir().unwrap();
        let source = crate::recovery::tests::source_fixture(bundle.path());
        let bundled = bundle
            .path()
            .join(maild_brand::APP_BUNDLE_NAME)
            .join("Contents/Resources/frontends")
            .join(crate::recovery::BUNDLED_FRONTEND_ID);
        let root = tempfile::tempdir().unwrap();
        source.preinstall(root.path()).unwrap();
        let installed = root.path().join(crate::recovery::BUNDLED_FRONTEND_ID);
        check_installed_bundled_frontend(&installed, &bundled).unwrap();

        // Same version, damaged bytes: native recovery, never the in-place window.
        let exe = crate::recovery::tests::ui_exe_in(&installed);
        let mut bytes = fs::read(&exe).unwrap();
        bytes[100] ^= 0xff;
        fs::write(&exe, &bytes).unwrap();
        assert!(check_installed_bundled_frontend(&installed, &bundled).is_err());
        assert!(installed_bundled_frontend_is_stale(&installed, &bundled));

        // A different installed version (pending upgrade/rollback) is not byte-compared.
        let manifest = installed.join(MANIFEST_FILE);
        let text = fs::read_to_string(&manifest).unwrap();
        fs::write(&manifest, text.replace("version = \"0.1.0\"", "version = \"0.0.9\""))
            .unwrap();
        check_installed_bundled_frontend(&installed, &bundled).unwrap();

        // Missing executable or a removed package is always rejected.
        fs::remove_file(&exe).unwrap();
        assert!(check_installed_bundled_frontend(&installed, &bundled).is_err());
        assert!(!installed_bundled_frontend_is_stale(&installed, &bundled));
        fs::remove_dir_all(&installed).unwrap();
        assert!(check_installed_bundled_frontend(&installed, &bundled).is_err());
    }
}
