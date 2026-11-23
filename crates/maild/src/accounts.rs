//! accounts：多账户装配（R4.5）+ 动态 AccountManager（M3.7 / ADR-0015）。
//!
//! 模式（对齐 rust-port-design.md §6 的多账户行）：
//! `let acct = ctx.scoped(r).isolate_at::<Receive>(r).isolate_at::<ReceiveConfig>(r);`
//! 每账户一个 realm：`Receive` / `ReceiveConfig` 独立绑定 + 事件 scope（账户事件只见本账户 + ROOT）。
//!
//! M3.7 演进：`Accounts` 从不可变 `Vec` 变为由 [`AccountManager`] 串行维护的快照服务；
//! 受管 profile 存独立版本化 `accounts.toml`（仅非秘密），秘密只进平台凭据库；
//! legacy `[[accounts]]` 只读兼容、editable=false；移除账户是软归档，本地邮件保留。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cordis::{Context, FiberHandle, RealmId, Service};
use maild_client::{TransportProbeRequest, TransportProbeResponse, TransportSendRequest};
use serde::{Deserialize, Serialize};

use crate::account_catalog::{
    self, AccountCatalog, AccountProfile, CatalogError, CredentialKey, CredentialRole,
    CredentialSharing, IncomingSettings, OutgoingSettings,
};
use crate::api::{ApiConfig, ApiPlugin};
use crate::credential::{CredentialError, CredentialStore};
use crate::external::{ExternalConfig, ExternalPlugin};
use crate::filters::{FiltersConfig, FiltersPlugin};
use crate::frontend::{FrontendConfig, FrontendPlugin};
use crate::loader::{load, plugin_config, Assembly, LoadError, Registry};
use crate::menu::{MenuConfig, MenuPlugin};
use crate::notify::{NotifyConfig, NotifyPlugin};
use crate::otp::{OtpConfig, OtpPlugin};
use crate::probe::{probe_incoming, probe_outgoing, IncomingProbe, OutgoingProbe, ProbeOutcome};
use crate::events::AccountDetailCode;
use crate::receive::{
    spawn_managed_receive, Receive, ReceiveConfig, ReceiveError, ReceivePlugin, SecretFetcher,
};
use crate::rules::{RulesConfig, RulesPlugin};
use crate::scheduler::{Scheduler, SchedulerConfig, SchedulerPlugin};
use crate::store::{StoreConfig, StorePlugin};
use crate::transport::{TransportConfig, TransportPlugin};

/// 探测与账户 mutation 共享的全局并发上限（ADR-0015 §8）。
const MAX_CONCURRENT_ACCOUNT_OPS: usize = 4;
/// 单侧探测端到端 deadline。
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);
/// 停止单个账户 runtime 的等待上限。
const RUNTIME_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

// —— Accounts 快照服务 ——

/// 账户列表服务（spine `ctx.accounts` 的最小形态）：(name, realm, 连接句柄)。
///
/// M3.7 起 entries 由 [`AccountManager`] 在运行时维护（create/update/disable/archive 时
/// 增量更新）；读取方一律走 [`Accounts::snapshot`]，锁内只做 O(1) clone。
pub struct Accounts {
    entries: Mutex<Vec<(String, RealmId, Receive)>>,
}

impl Accounts {
    #[allow(dead_code)]
    pub(crate) fn with_entries(entries: Vec<(String, RealmId, Receive)>) -> Self {
        Self {
            entries: Mutex::new(entries),
        }
    }

    /// 当前账户句柄快照（legacy + 受管）。
    pub fn snapshot(&self) -> Vec<(String, RealmId, Receive)> {
        self.entries.lock().unwrap().clone()
    }

    fn upsert(&self, name: &str, realm: RealmId, receive: Receive) {
        let mut entries = self.entries.lock().unwrap();
        match entries.iter_mut().find(|(existing, _, _)| existing == name) {
            Some(slot) => slot.2 = receive,
            None => entries.push((name.to_string(), realm, receive)),
        }
    }

    fn remove(&self, name: &str) {
        self.entries
            .lock()
            .unwrap()
            .retain(|(existing, _, _)| existing != name);
    }
}

impl Service for Accounts {
    fn name(&self) -> &'static str {
        "accounts"
    }
}

// —— 全局插件注册表与装配入口 ——

/// 全局插件注册表（filters / rules / store / frontend / notify / otp / transport / api / scheduler）。
/// receive 按账户装配，不在此列。
pub fn global_registry() -> Registry {
    let mut reg = Registry::default();
    reg.register(crate::loader::plugin::<FiltersConfig, FiltersPlugin, _>(
        "filters",
        |_: &Context, _: Arc<FiltersConfig>| Arc::new(FiltersPlugin),
    ));
    reg.register(crate::loader::plugin::<RulesConfig, RulesPlugin, _>(
        "rules",
        |_: &Context, cfg: Arc<RulesConfig>| Arc::new(RulesPlugin::new(&cfg)),
    ));
    reg.register(crate::loader::plugin::<StoreConfig, StorePlugin, _>(
        "store",
        |_: &Context, _: Arc<StoreConfig>| Arc::new(StorePlugin),
    ));
    reg.register(crate::loader::plugin::<ExternalConfig, ExternalPlugin, _>(
        "external",
        |_: &Context, _: Arc<ExternalConfig>| Arc::new(ExternalPlugin),
    ));
    reg.register(crate::loader::plugin::<FrontendConfig, FrontendPlugin, _>(
        "frontend",
        |_: &Context, _: Arc<FrontendConfig>| Arc::new(FrontendPlugin),
    ));
    reg.register(crate::loader::plugin::<MenuConfig, MenuPlugin, _>(
        "menu",
        |_: &Context, _: Arc<MenuConfig>| Arc::new(MenuPlugin),
    ));
    reg.register(crate::loader::plugin::<NotifyConfig, NotifyPlugin, _>(
        "notify",
        |_: &Context, _: Arc<NotifyConfig>| Arc::new(NotifyPlugin),
    ));
    reg.register(crate::loader::plugin::<OtpConfig, OtpPlugin, _>(
        "otp",
        |_: &Context, _: Arc<OtpConfig>| Arc::new(OtpPlugin),
    ));
    reg.register(
        crate::loader::plugin::<TransportConfig, TransportPlugin, _>(
            "transport",
            |_: &Context, _: Arc<TransportConfig>| Arc::new(TransportPlugin),
        ),
    );
    reg.register(crate::loader::plugin::<ApiConfig, ApiPlugin, _>(
        "api",
        |_: &Context, _: Arc<ApiConfig>| Arc::new(ApiPlugin),
    ));
    reg.register(
        crate::loader::plugin::<SchedulerConfig, SchedulerPlugin, _>(
            "scheduler",
            |_: &Context, _: Arc<SchedulerConfig>| Arc::new(SchedulerPlugin),
        ),
    );
    reg
}

/// 受管账户目录的运行参数（仅平台默认配置时由 paths 层提供）。
#[derive(Clone, Debug)]
pub struct ManagedAccountsOptions {
    /// 版本化 `accounts.toml` 的绝对路径。
    pub catalog_path: PathBuf,
}

/// 装配入口：`[plugins]` 全局插件 + `[[accounts]]` 每账户 receive。
/// 无 `accounts` 节 = 纯全局插件装配（兼容旧行为）。
pub fn assemble(ctx: &Context, toml_text: &str) -> Result<Assembly, LoadError> {
    assemble_with_options(ctx, toml_text, None)
}

/// 带受管账户目录的装配入口（生产 Agent 平台默认配置路径）。
pub fn assemble_with_options(
    ctx: &Context,
    toml_text: &str,
    managed: Option<ManagedAccountsOptions>,
) -> Result<Assembly, LoadError> {
    validate_global_configs(toml_text)?;
    let account_configs = parse_account_configs(toml_text)?;

    // 受管目录在任何网络任务挂载前完成解析与冲突检查（fail-closed，不清理凭据）。
    let catalog = match &managed {
        Some(options) => match account_catalog::load_catalog(&options.catalog_path) {
            Ok(catalog) => catalog.unwrap_or_else(AccountCatalog::empty),
            Err(error) => return Err(LoadError::Structure(error.to_string())),
        },
        None => AccountCatalog::empty(),
    };
    if let Some(conflict) = account_configs
        .iter()
        .find(|cfg| catalog.profile(&cfg.account).is_some())
    {
        return Err(LoadError::Structure(format!(
            "旧 [[accounts]] 名称 {} 与受管账户 id 冲突；请改名后重试",
            conflict.account
        )));
    }

    let mut asm = load(&global_registry(), ctx, toml_text)?;
    let entries = mount_account_configs(ctx, account_configs.clone(), &mut asm);

    let accounts = Arc::new(Accounts::with_entries(entries));
    match managed {
        Some(options) => {
            let manager = AccountManager::start(ctx, options, catalog, accounts.clone(), account_configs)
                .map_err(LoadError::Structure)?;
            ctx.provide::<AccountManager>(manager);
            ctx.provide::<Accounts>(accounts);
        }
        None => {
            if !accounts.snapshot().is_empty() {
                ctx.provide::<Accounts>(accounts);
            }
        }
    }
    Ok(asm)
}

/// Validate every account before any global plugin is mounted.
///
/// Production path preparation calls this before creating managed directories; [`assemble`]
/// calls it before `load`, so a fatal account shape error cannot strand Store/API resources.
pub(crate) fn validate_accounts(toml_text: &str) -> Result<(), LoadError> {
    validate_global_configs(toml_text)?;
    parse_account_configs(toml_text).map(|_| ())
}

fn validate_global_configs(toml_text: &str) -> Result<(), LoadError> {
    if let Some(config) = plugin_config::<RulesConfig>(toml_text, "rules")? {
        crate::rules::validate_config(&config).map_err(|detail| LoadError::Apply {
            plugin: "rules".into(),
            detail,
        })?;
    }
    if let Some(config) = plugin_config::<NotifyConfig>(toml_text, "notify")? {
        crate::notify::validate_config(&config).map_err(|detail| LoadError::Apply {
            plugin: "notify".into(),
            detail,
        })?;
    }
    if let Some(config) = plugin_config::<FrontendConfig>(toml_text, "frontend")? {
        crate::frontend::validate_config(&config).map_err(|detail| LoadError::Apply {
            plugin: "frontend".into(),
            detail,
        })?;
    }
    if let Some(config) = plugin_config::<ExternalConfig>(toml_text, "external")? {
        crate::external::validate_config(&config).map_err(|detail| LoadError::Apply {
            plugin: "external".into(),
            detail,
        })?;
    }
    if let Some(config) = plugin_config::<MenuConfig>(toml_text, "menu")? {
        crate::menu::validate_config(&config).map_err(|detail| LoadError::Apply {
            plugin: "menu".into(),
            detail,
        })?;
    }
    Ok(())
}

/// 逐账户装配 receive：scoped + isolate（Receive / ReceiveConfig 独立绑定），
/// provide 该账户配置后挂载 ReceivePlugin。
pub fn mount_accounts(
    ctx: &Context,
    toml_text: &str,
    asm: &mut Assembly,
) -> Result<Vec<(String, RealmId, Receive)>, LoadError> {
    let configs = parse_account_configs(toml_text)?;
    Ok(mount_account_configs(ctx, configs, asm))
}

fn parse_account_configs(toml_text: &str) -> Result<Vec<ReceiveConfig>, LoadError> {
    let root: toml::Value = toml::from_str(toml_text).map_err(LoadError::Parse)?;
    let Some(accounts) = root.get("accounts") else {
        return Ok(Vec::new());
    };
    let Some(arr) = accounts.as_array() else {
        return Err(LoadError::Structure(
            "`accounts` 必须是表数组（[[accounts]]）".into(),
        ));
    };
    let mut configs = Vec::with_capacity(arr.len());
    let mut names = HashSet::new();
    for (i, entry) in arr.iter().enumerate() {
        let cfg: ReceiveConfig = entry
            .clone()
            .try_into()
            .map_err(|source| LoadError::Config {
                plugin: format!("accounts[{i}]"),
                source,
            })?;
        if cfg.account.is_empty() {
            return Err(LoadError::Structure(format!(
                "accounts[{i}] 缺少 account 名"
            )));
        }
        if !names.insert(cfg.account.clone()) {
            return Err(LoadError::Structure(format!(
                "accounts[{i}] 重复 account 名: {}",
                cfg.account
            )));
        }
        configs.push(cfg);
    }

    Ok(configs)
}

fn mount_account_configs(
    ctx: &Context,
    configs: Vec<ReceiveConfig>,
    asm: &mut Assembly,
) -> Vec<(String, RealmId, Receive)> {
    let mut entries = Vec::new();
    for cfg in configs {
        let realm = ctx.alloc_realm();
        let acct = ctx
            .scoped(realm)
            .isolate_at::<Receive>(realm)
            .isolate_at::<ReceiveConfig>(realm);
        acct.provide::<ReceiveConfig>(Arc::new(cfg.clone()));
        let handle: FiberHandle = acct.plugin(Arc::new(ReceivePlugin));
        let Some(receive) = acct.get::<Receive>() else {
            asm.errors.push(LoadError::Apply {
                plugin: format!("receive:{}", cfg.account),
                detail: "apply 未产出 Receive（需在 tokio 运行时内装配）".into(),
            });
            continue;
        };
        asm.handles
            .push((format!("receive:{}", cfg.account), handle));
        entries.push((cfg.account.clone(), realm, (*receive).clone()));
    }
    entries
}

// —— 动态 AccountManager（ADR-0015 §6/§7） ——

/// 账户管理错误。api 层负责映射为稳定 JSON-RPC 错误码。
#[derive(Debug)]
pub enum ManagerError {
    NotFound,
    Conflict(String),
    ProbeFailed(ProbeReport),
    Credential(CredentialError),
    Invalid(String),
    Unavailable(String),
    /// 委托 transport 的失败：(错误码 -32018/-32019/-32020, 脱敏 detail)。
    Provider(i64, String),
    Internal(String),
    Delivery(crate::transport::DeliveryFailure),
}

impl std::fmt::Display for ManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManagerError::NotFound => f.write_str("账户不存在或已不可操作"),
            ManagerError::Conflict(detail) => write!(f, "账户状态冲突: {detail}"),
            ManagerError::ProbeFailed(_) => f.write_str("连接探测未通过"),
            ManagerError::Credential(error) => write!(f, "凭据库不可用: {error}"),
            ManagerError::Invalid(detail) => write!(f, "账户参数无效: {detail}"),
            ManagerError::Unavailable(detail) => write!(f, "账户服务不可用: {detail}"),
            ManagerError::Provider(_, detail) => write!(f, "委托 transport 失败: {detail}"),
            ManagerError::Delivery(error) => write!(f, "发送未完成: {}", error.detail),
            ManagerError::Internal(detail) => write!(f, "账户服务内部错误: {detail}"),
        }
    }
}

impl From<CatalogError> for ManagerError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::FutureVersion(version) => {
                ManagerError::Conflict(format!("账户目录 format_version {version} 不受支持"))
            }
            other => ManagerError::Internal(other.to_string()),
        }
    }
}

/// 创建/探测用的账户草稿（preset 已在前端解析为显式 endpoint）。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountDraft {
    pub display_name: String,
    pub address: String,
    pub incoming: IncomingSettings,
    #[serde(default)]
    pub outgoing: Option<OutgoingSettings>,
}

/// 一次性提交的秘密。JSON 形如 `{ "incoming": "…", "outgoing": "…" }`；
/// Debug 永不回显内容，序列化被显式禁止（秘密只写不读）。
#[derive(Clone, Default)]
pub struct AccountCredentials {
    pub incoming: Vec<u8>,
    pub outgoing: Option<Vec<u8>>,
}

impl<'de> Deserialize<'de> for AccountCredentials {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            #[serde(default)]
            incoming: Option<String>,
            #[serde(default)]
            outgoing: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            incoming: raw.incoming.map(String::into_bytes).unwrap_or_default(),
            outgoing: raw.outgoing.map(String::into_bytes),
        })
    }
}

impl std::fmt::Debug for AccountCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountCredentials")
            .field("incoming", &format!("[{} bytes]", self.incoming.len()))
            .field(
                "outgoing",
                &self
                    .outgoing
                    .as_ref()
                    .map(|secret| format!("[{} bytes]", secret.len())),
            )
            .finish()
    }
}

/// 部分更新。`outgoing` 双层 Option：缺省=不变，null=移除发信，对象=替换。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountPatch {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub incoming: Option<IncomingSettings>,
    #[serde(default, deserialize_with = "deserialize_outgoing_patch")]
    pub outgoing: Option<Option<OutgoingSettings>>,
}

fn deserialize_outgoing_patch<'de, D: serde::Deserializer<'de>>(deserializer: D)
    -> Result<Option<Option<OutgoingSettings>>, D::Error> {
    // This hook only runs for a present key. Serde's nested Option otherwise collapses null
    // and omission, silently keeping SMTP when a client explicitly removes it.
    Option::<OutgoingSettings>::deserialize(deserializer).map(Some)
}

/// probeAccount 的双侧结果（不保存）。
#[derive(Clone, Debug, Serialize)]
pub struct ProbeReport {
    pub incoming: ProbeOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<ProbeOutcome>,
}

/// removeAccount 结果。
#[derive(Clone, Debug, Serialize)]
pub struct RemoveOutcome {
    pub archived: bool,
    pub credentials_deleted: bool,
    pub messages_retained: bool,
}

struct ManagedRuntime {
    receive: Receive,
    task: tokio::task::JoinHandle<()>,
}

struct ManagerState {
    catalog: AccountCatalog,
    /// 插入序保持 LIFO 关停语义。
    runtimes: Vec<(String, ManagedRuntime)>,
    /// legacy 只读条目：(名称, 原始配置, realm, 连接句柄)。
    legacy: Vec<(String, ReceiveConfig, RealmId, Receive)>,
}

/// 动态账户管理器：账户目录、凭据访问和运行时生命周期的唯一写入者。
pub struct AccountManager {
    ctx: Context,
    store: Arc<dyn CredentialStore>,
    catalog_path: PathBuf,
    mutation_lock: tokio::sync::Mutex<()>,
    permits: Arc<tokio::sync::Semaphore>,
    state: Mutex<ManagerState>,
    accounts: Arc<Accounts>,
    shutting_down: AtomicBool,
}

impl Service for AccountManager {
    fn name(&self) -> &'static str {
        "account.manager"
    }
}

impl AccountManager {
    /// 启动受管账户：挂载已启用 profile、收敛孤儿凭据并注册关停逆序。
    pub(crate) fn start(
        ctx: &Context,
        options: ManagedAccountsOptions,
        catalog: AccountCatalog,
        accounts: Arc<Accounts>,
        legacy_configs: Vec<ReceiveConfig>,
    ) -> Result<Arc<Self>, String> {
        let store = crate::credential::platform_store_for(&options.catalog_path);
        let snapshot = accounts.snapshot();
        let legacy = legacy_configs
            .into_iter()
            .filter_map(|cfg| {
                let name = cfg.account.clone();
                // mount_account_configs 已把同名句柄放进快照；realm 取自该条目。
                snapshot
                    .iter()
                    .find(|(entry_name, _, _)| entry_name == &name)
                    .map(|(_, realm, receive)| (name, cfg, *realm, receive.clone()))
            })
            .collect();

        let manager = Arc::new(Self {
            ctx: ctx.clone(),
            store,
            catalog_path: options.catalog_path,
            mutation_lock: tokio::sync::Mutex::new(()),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_ACCOUNT_OPS)),
            state: Mutex::new(ManagerState {
                catalog,
                runtimes: Vec::new(),
                legacy,
            }),
            accounts,
            shutting_down: AtomicBool::new(false),
        });

        // 先收敛孤儿凭据与归档账户的地址残留（目录解析成功后才允许
        // 清理），再挂载启用的账户。
        manager.sweep_orphan_credentials();
        manager.sweep_archived_addresses();
        let enabled: Vec<AccountProfile> = {
            let state = manager.state.lock().unwrap();
            state
                .catalog
                .profiles
                .iter()
                .filter(|profile| profile.enabled && profile.archived_at.is_none())
                .cloned()
                .collect()
        };
        for profile in enabled {
            if let Err(error) = manager.mount_runtime(&profile) {
                eprintln!(
                    "[maild/accounts] 账户 {} 启动失败: {}",
                    truncate_short(&profile.id),
                    error
                );
            }
        }

        // 关停逆序：manager 的 effect 最后注册，teardown 最先执行——
        // 先拒绝新的写请求，再按插入序的逆序释放所有受管 runtime。
        let teardown = manager.clone();
        ctx.effect_async(async move {
            teardown.shutdown_all().await;
        });
        Ok(manager)
    }

    /// 孤儿凭据收敛：只删除本 service 下、命名可解析且不被任何当前/归档 profile 引用的 item。
    fn sweep_orphan_credentials(&self) {
        let referenced: HashSet<String> = {
            let state = self.state.lock().unwrap();
            state
                .catalog
                .profiles
                .iter()
                .flat_map(|profile| {
                    [
                        profile.incoming_credential_revision.map(|revision| {
                            CredentialKey::new(
                                profile.id.clone(),
                                CredentialRole::Incoming,
                                revision,
                            )
                            .attr_account()
                        }),
                        profile.outgoing_credential_revision.map(|revision| {
                            CredentialKey::new(
                                profile.id.clone(),
                                CredentialRole::Outgoing,
                                revision,
                            )
                            .attr_account()
                        }),
                    ]
                    .into_iter()
                    .flatten()
                })
                .collect()
        };
        let attrs = match self.store.list_attrs() {
            Ok(attrs) => attrs,
            Err(error) => {
                eprintln!("[maild/accounts] 凭据孤儿收敛跳过: {error}");
                return;
            }
        };
        for attr in attrs {
            if referenced.contains(&attr) {
                continue;
            }
            let Some(key) = CredentialKey::parse(&attr) else {
                continue; // 无法证明归属本产品的遗留项不动
            };
            if let Err(error) = self.store.delete(&key) {
                eprintln!("[maild/accounts] 删除孤儿凭据失败: {error}");
            }
        }
    }

    /// 收敛 removeAccount 在「catalog 已发布归档、SQLite DELETE 尚未完成」
    /// 之间异常退出留下的地址。只处理目录明确标记为 archived 的账户；
    /// 活跃/未知父账户一律不碰。
    fn sweep_archived_addresses(&self) {
        let archived: Vec<String> = {
            let state = self.state.lock().unwrap();
            state
                .catalog
                .profiles
                .iter()
                .filter(|profile| profile.archived_at.is_some())
                .map(|profile| profile.id.clone())
                .collect()
        };
        let Some(store) = self.ctx.get::<crate::store::Store>() else {
            return;
        };
        for account in archived {
            match store.delete_addresses_for_account(&account) {
                Ok(removed) if removed > 0 => eprintln!(
                    "[maild/accounts] 启动收敛归档账户地址 account={} removed={removed}",
                    account
                ),
                Ok(_) => {}
                Err(error) => eprintln!(
                    "[maild/accounts] 启动收敛归档账户地址失败 account={}: {error}",
                    account
                ),
            }
        }
    }

    fn build_receive_config(profile: &AccountProfile) -> ReceiveConfig {
        let (server, port, security, username, mailbox, idle, poll_secs) =
            profile.incoming.imap().expect("仅 Imap 变体进入内置收信");
        ReceiveConfig {
            account: profile.id.clone(),
            server: server.to_string(),
            port,
            username: username.to_string(),
            password: String::new(), // 秘密经 SecretFetcher 短生命周期获取
            mailbox: mailbox.to_string(),
            reconnect_secs: 5,
            idle,
            poll_secs,
            folder_poll_secs: 300,
            tls: security.as_str().to_string(),
            accept_invalid_certs: false,
            initial_sync_limit: crate::receive::DEFAULT_INITIAL_SYNC_LIMIT,
        }
    }

    fn incoming_fetcher(&self, profile: &AccountProfile) -> Result<SecretFetcher, ManagerError> {
        let revision = profile
            .incoming_credential_revision
            .ok_or_else(|| ManagerError::Invalid("账户缺少收信凭据引用".into()))?;
        let key = CredentialKey::new(profile.id.clone(), CredentialRole::Incoming, revision);
        let store = self.store.clone();
        Ok(Arc::new(move || {
            // 凭据库的报错是我们自己的文字，不作为 `server_detail` 透出（ADR-0048 L6）。
            store.load(&key).map_err(|error| {
                ReceiveError::new(
                    AccountDetailCode::CredentialUnavailable,
                    format!("凭据读取失败: {error}"),
                )
            })
        }))
    }

    /// 挂载（或重挂载）一个受管账户的收信 runtime，并同步 Accounts 快照。
    /// incoming 委托给插件（ADR-0021）时走 provider 轮询循环；否则按 idle/poll
    /// 选择 IMAP 路径。
    fn mount_runtime(&self, profile: &AccountProfile) -> Result<(), ManagerError> {
        let realm = self.ctx.alloc_realm();
        let (receive, task) = match &profile.incoming {
            IncomingSettings::Imap { .. } => {
                let fetcher = self.incoming_fetcher(profile)?;
                let (_, _, _, _, _, idle, _) = profile.incoming.imap().expect("Imap 变体已匹配");
                let scheduler = if idle {
                    None
                } else {
                    Some(self.ctx.get::<Scheduler>().ok_or_else(|| {
                        ManagerError::Internal("轮询账户需要 scheduler 插件".into())
                    })?.as_ref().clone())
                };
                spawn_managed_receive(
                    &self.ctx,
                    realm,
                    Self::build_receive_config(profile),
                    fetcher,
                    scheduler,
                )
                .map_err(ManagerError::Internal)?
            }
            IncomingSettings::Plugin { provider, poll_secs } => {
                crate::receive::spawn_provider_receive(
                    &self.ctx,
                    realm,
                    crate::receive::ProviderReceiveSpec {
                        account: profile.id.clone(),
                        address: profile.address.clone(),
                        provider_id: provider.clone(),
                        poll_secs: *poll_secs,
                    },
                )
                .map_err(ManagerError::Internal)?
            }
        };
        {
            let mut state = self.state.lock().unwrap();
            state.runtimes.retain(|(id, _)| id != &profile.id);
            state.runtimes.push((
                profile.id.clone(),
                ManagedRuntime {
                    receive: receive.clone(),
                    task,
                },
            ));
        }
        self.accounts.upsert(&profile.id, realm, receive);
        Ok(())
    }

    /// 取出某账户的 runtime（从状态中移除）。
    fn take_runtime(&self, id: &str) -> Option<ManagedRuntime> {
        let mut state = self.state.lock().unwrap();
        state
            .runtimes
            .iter()
            .position(|(existing, _)| existing == id)
            .map(|index| state.runtimes.remove(index).1)
    }

    /// 停止一个 runtime 并等待退出。
    async fn stop_runtime(runtime: ManagedRuntime) {
        runtime.receive.shutdown();
        let _ = tokio::time::timeout(RUNTIME_STOP_TIMEOUT, runtime.task).await;
    }

    /// 关停全部受管 runtime（先拒绝新写请求，再按插入序逆序释放）。
    pub async fn shutdown_all(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        let runtimes: Vec<ManagedRuntime> = {
            let mut state = self.state.lock().unwrap();
            state
                .runtimes
                .drain(..)
                .map(|(_, runtime)| runtime)
                .collect()
        };
        for runtime in runtimes.into_iter().rev() {
            Self::stop_runtime(runtime).await;
        }
    }

    fn reject_if_shutting_down(&self) -> Result<(), ManagerError> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(ManagerError::Unavailable("账户服务正在关闭".into()));
        }
        Ok(())
    }

    async fn acquire_permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit, ManagerError> {
        self.permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ManagerError::Internal("并发信号量已关闭".into()))
    }

    // —— 目录发布 ——

    /// 在锁内修改目录并原子发布；返回发布前的目录快照用于回滚。
    async fn publish_with<F>(&self, mutate: F) -> Result<AccountCatalog, ManagerError>
    where
        F: FnOnce(&mut AccountCatalog),
    {
        let previous = self.state.lock().unwrap().catalog.clone();
        let next = {
            let mut state = self.state.lock().unwrap();
            mutate(&mut state.catalog);
            state.catalog.clone()
        };
        if let Err(error) = account_catalog::save_catalog(&self.catalog_path, &next) {
            // 文件发布失败：回滚内存目录到发布前状态。
            self.state.lock().unwrap().catalog = previous;
            return Err(error.into());
        }
        Ok(previous)
    }

    fn restore_catalog(&self, previous: AccountCatalog) -> Result<(), ManagerError> {
        account_catalog::save_catalog(&self.catalog_path, &previous).map_err(ManagerError::from)?;
        self.state.lock().unwrap().catalog = previous;
        Ok(())
    }

    // —— 视图 ——

    /// 非秘密 profile 视图（含凭据状态与运行状态），秘密绝不出现。
    pub fn profiles_view(&self, include_archived: bool) -> Vec<ProfileView> {
        let state = self.state.lock().unwrap();
        let mut views: Vec<ProfileView> = Vec::new();
        for profile in &state.catalog.profiles {
            if !include_archived && profile.archived_at.is_some() {
                continue;
            }
            let runtime = state
                .runtimes
                .iter()
                .find(|(id, _)| id == &profile.id)
                .map(|(_, runtime)| runtime.receive.state());
            views.push(self.managed_view_locked(profile, runtime, &state));
        }
        for (name, cfg, _, receive) in &state.legacy {
            views.push(legacy_view(name, cfg, receive.state()));
        }
        views
    }

    fn credential_state(&self, key: Option<CredentialKey>) -> String {
        match key {
            None => "missing".into(),
            Some(key) => match self.store.contains(&key) {
                Ok(true) => "saved".into(),
                Ok(false) => "missing".into(),
                Err(_) => "unknown".into(),
            },
        }
    }

    /// 调用方必须已持有 `state` 锁（视图组装不做网络 IO，只有凭据存在性查询）。
    fn managed_view_locked(
        &self,
        profile: &AccountProfile,
        runtime: Option<crate::events::AccountStateInfo>,
        _state: &ManagerState,
    ) -> ProfileView {
        let incoming_view = match &profile.incoming {
            IncomingSettings::Imap { .. } => {
                let (server, port, security, username, mailbox, idle, poll_secs) =
                    profile.incoming.imap().expect("Imap 变体已匹配");
                IncomingView {
                    kind: "imap",
                    server: server.to_string(),
                    port,
                    security: security.as_str().to_string(),
                    username: username.to_string(),
                    mailbox: mailbox.to_string(),
                    idle,
                    poll_secs,
                    provider: None,
                }
            }
            IncomingSettings::Plugin { provider, poll_secs } => IncomingView {
                kind: "plugin",
                server: String::new(),
                port: 0,
                security: String::new(),
                username: String::new(),
                mailbox: String::new(),
                idle: false,
                poll_secs: *poll_secs,
                provider: Some(provider.clone()),
            },
        };
        let incoming_key = profile.incoming_credential_revision.map(|revision| {
            CredentialKey::new(profile.id.clone(), CredentialRole::Incoming, revision)
        });
        let outgoing_key = profile.outgoing_credential_revision.map(|revision| {
            CredentialKey::new(profile.id.clone(), CredentialRole::Outgoing, revision)
        });
        let outgoing_view = profile.outgoing.as_ref().map(|outgoing| match outgoing {
            OutgoingSettings::Smtp { .. } => {
                let (server, port, security, username, credential) =
                    outgoing.smtp().expect("Smtp 变体已匹配");
                (
                    OutgoingView {
                        kind: "smtp",
                        server: server.to_string(),
                        port,
                        security: security.as_str().to_string(),
                        username: username.to_string(),
                        credential: match credential {
                            CredentialSharing::Incoming => "incoming".to_string(),
                            CredentialSharing::Separate => "separate".to_string(),
                        },
                        provider: None,
                    },
                    matches!(credential, CredentialSharing::Separate),
                )
            }
            OutgoingSettings::Plugin { provider } => (
                OutgoingView {
                    kind: "plugin",
                    server: String::new(),
                    port: 0,
                    security: String::new(),
                    username: String::new(),
                    credential: String::new(),
                    provider: Some(provider.clone()),
                },
                false,
            ),
        });
        let credentials = CredentialsView {
            incoming: self.credential_state(incoming_key),
            outgoing: outgoing_view.as_ref().map(|(_, separate)| {
                if *separate {
                    self.credential_state(outgoing_key)
                } else {
                    "shared".to_string()
                }
            }),
        };
        let delegated_incoming = matches!(profile.incoming, IncomingSettings::Plugin { .. });
        let runtime_state = match (&runtime, profile.enabled) {
            (None, false) => RuntimeState::Disabled,
            // 委托账户没有凭据语义：无 runtime 信息时按离线展示而非缺凭据。
            (None, true) if delegated_incoming => RuntimeState::Error,
            (None, true) => RuntimeState::NeedsCredentials,
            (Some(info), _) => match info.state {
                crate::events::AccountStateKind::Connecting => RuntimeState::Connecting,
                crate::events::AccountStateKind::Online => RuntimeState::Online,
                crate::events::AccountStateKind::Reconnecting => RuntimeState::Reconnecting,
                crate::events::AccountStateKind::Offline => {
                    if !delegated_incoming && credentials.incoming != "saved" {
                        RuntimeState::NeedsCredentials
                    } else {
                        RuntimeState::Error
                    }
                }
            },
        };
        ProfileView {
            id: profile.id.clone(),
            revision: profile.revision,
            display_name: profile.display_name.clone(),
            address: profile.address.clone(),
            enabled: profile.enabled,
            archived_at: profile.archived_at,
            source: "managed",
            editable: true,
            incoming: incoming_view,
            outgoing: outgoing_view.map(|(view, _)| view),
            credentials,
            runtime: RuntimeView::from_info(runtime_state.label(), runtime.as_ref()),
        }
    }

    // —— 探测 ——

    /// 委托方向的 provider 端到端探测（M4-05/M4-07，ADR-0021/0023）。draft
    /// 阶段尚无账户 id，因此 `account` 为空；更新既有账户时传真实 id，让有
    /// 持久绑定的 provider 在目录发布前拒绝不安全的重绑定。`address` 是 provider
    /// 定位远端邮箱所需的稳定地址，不能从 alias `from` 反推。
    async fn provider_probe(
        &self,
        provider_id: &str,
        account: &str,
        address: &str,
        direction: &str,
    ) -> ProbeOutcome {
        let outcome = |ok, detail: String| ProbeOutcome::provider_outcome(ok, detail);
        let Some(registry) = self.ctx.get::<crate::provider::ProviderRegistry>() else {
            return outcome(false, "provider 注册表未装配".into());
        };
        let Some(endpoint) = registry.endpoint_of(provider_id) else {
            return outcome(false, format!("provider {provider_id} 未注册"));
        };
        match endpoint
            .call(
                "transport/probe",
                serde_json::to_value(TransportProbeRequest {
                    account: account.to_owned(),
                    address: address.to_owned(),
                    direction: direction.to_owned(),
                })
                .expect("transport probe DTO is serializable"),
                crate::provider::probe_budget(),
            )
            .await
        {
            Ok(value) => match serde_json::from_value::<TransportProbeResponse>(value) {
                Ok(response) if response.stage == "provider" => {
                    outcome(response.ok, response.detail)
                }
                _ => outcome(false, "provider 探测应答无效".into()),
            },
            Err(crate::provider::ProviderCallError::Timeout) => {
                outcome(false, "provider 响应超时".into())
            }
            Err(crate::provider::ProviderCallError::Unavailable(detail)) => {
                outcome(false, format!("provider 不可用: {detail}"))
            }
            Err(crate::provider::ProviderCallError::Upstream { message, .. }) => {
                outcome(false, message)
            }
        }
    }

    /// 双侧并发探测；任一侧失败不影响另一侧结果。委托方向改走 provider。
    /// 注意：守护进程使用 current_thread 运行时，禁止 block_in_place，直接 await。
    async fn draft_probe_report(
        &self,
        account: &str,
        draft: &AccountDraft,
        incoming_secret: &[u8],
        outgoing_secret: Option<&[u8]>,
    ) -> ProbeReport {
        let outgoing_side = async {
            match &draft.outgoing {
                None => None,
                Some(OutgoingSettings::Plugin { provider }) => {
                    Some(
                        self.provider_probe(provider, account, &draft.address, "outgoing")
                            .await,
                    )
                }
                Some(outgoing @ OutgoingSettings::Smtp { .. }) => {
                    let (server, port, security, username, _sharing) =
                        outgoing.smtp().expect("Smtp 变体已匹配");
                    Some(
                        probe_outgoing(
                            OutgoingProbe {
                                server,
                                port,
                                security,
                                username,
                                secret: outgoing_secret,
                            },
                            PROBE_DEADLINE,
                        )
                        .await,
                    )
                }
            }
        };
        let incoming_side = async {
            match &draft.incoming {
                IncomingSettings::Imap { .. } => {
                    let (server, port, security, username, mailbox, idle, _) =
                        draft.incoming.imap().expect("Imap 变体已匹配");
                    probe_incoming(
                        IncomingProbe {
                            server,
                            port,
                            security,
                            username,
                            secret: incoming_secret,
                            mailbox,
                            idle,
                        },
                        PROBE_DEADLINE,
                    )
                    .await
                }
                IncomingSettings::Plugin { provider, .. } => {
                    self.provider_probe(provider, account, &draft.address, "incoming")
                        .await
                }
            }
        };
        let (incoming, outgoing) = tokio::join!(incoming_side, outgoing_side);
        ProbeReport { incoming, outgoing }
    }

    /// probeAccount：双侧分阶段探测，不保存任何状态。
    pub async fn probe_account(
        &self,
        draft: &AccountDraft,
        credentials: &AccountCredentials,
    ) -> Result<ProbeReport, ManagerError> {
        self.reject_if_shutting_down()?;
        self.validate_draft(draft, credentials)?;
        let _permit = self.acquire_permit().await?;
        let outgoing_secret = self.draft_outgoing_secret(draft, credentials)?;
        Ok(self
            .draft_probe_report("", draft, &credentials.incoming, outgoing_secret.as_deref())
            .await)
    }

    /// An existing provider binding must be probed under its real account identity. The revision
    /// is checked on both sides of the network wait; this method never publishes configuration.
    pub async fn probe_existing_account(
        &self, id: &str, expected_revision: u64,
        draft: &AccountDraft, credentials: &AccountCredentials,
    ) -> Result<ProbeReport, ManagerError> {
        self.reject_if_shutting_down()?;
        self.validate_draft(draft, credentials)?;
        let _permit = self.acquire_permit().await?;
        let check_revision = || {
            let profile = self.require_live_profile(id)?;
            if profile.revision != expected_revision { return Err(ManagerError::Conflict("探测期间账户版本已变化".into())); }
            Ok(())
        };
        check_revision()?;
        let outgoing_secret = self.draft_outgoing_secret(draft, credentials)?;
        let report = self.draft_probe_report(id, draft, &credentials.incoming, outgoing_secret.as_deref()).await;
        check_revision()?;
        Ok(report)
    }

    /// 发信探测用的秘密：SMTP separate 取独立凭据，复用取收信凭据，
    /// 委托（plugin）方向无凭据。
    fn draft_outgoing_secret(
        &self,
        draft: &AccountDraft,
        credentials: &AccountCredentials,
    ) -> Result<Option<Vec<u8>>, ManagerError> {
        match &draft.outgoing {
            None => Ok(None),
            Some(outgoing) => match outgoing.smtp() {
                Some((_, _, _, _, sharing)) => Ok(Some(match sharing {
                    CredentialSharing::Incoming => credentials.incoming.clone(),
                    CredentialSharing::Separate => credentials
                        .outgoing
                        .clone()
                        .ok_or_else(|| ManagerError::Invalid("separate 发信凭据缺失".into()))?,
                })),
                // Plugin 委托：凭据不跨缝。
                None => Ok(None),
            },
        }
    }

    fn validate_draft(
        &self,
        draft: &AccountDraft,
        credentials: &AccountCredentials,
    ) -> Result<(), ManagerError> {
        let delegated_incoming = matches!(draft.incoming, IncomingSettings::Plugin { .. });
        let delegated_outgoing = matches!(
            &draft.outgoing,
            Some(OutgoingSettings::Plugin { .. })
        );
        // 委托方向不保留凭据引用，也不接受提交的秘密（凭据不跨缝）。
        if delegated_incoming && !credentials.incoming.is_empty() {
            return Err(ManagerError::Invalid("incoming 委托给插件时不接受凭据".into()));
        }
        if delegated_outgoing && credentials.outgoing.is_some() {
            return Err(ManagerError::Invalid("outgoing 委托给插件时不接受凭据".into()));
        }
        let candidate = AccountProfile {
            id: VALIDATION_ONLY_ID.into(),
            revision: 1,
            display_name: draft.display_name.clone(),
            address: draft.address.clone(),
            enabled: true,
            archived_at: None,
            incoming_credential_revision: if delegated_incoming { None } else { Some(1) },
            outgoing_credential_revision: None,
            incoming: draft.incoming.clone(),
            outgoing: draft.outgoing.clone(),
        };
        account_catalog::validate_profile(&candidate).map_err(ManagerError::Invalid)?;
        if !delegated_incoming {
            validate_secret(&credentials.incoming)?;
        }
        if let Some(outgoing) = &draft.outgoing {
            if matches!(outgoing.smtp(), Some((_, _, _, _, CredentialSharing::Separate))) {
                validate_secret(
                    credentials
                        .outgoing
                        .as_deref()
                        .ok_or_else(|| ManagerError::Invalid("separate 发信凭据缺失".into()))?,
                )?;
            }
        }
        Ok(())
    }

    // —— create ——

    /// createAccount：校验 → 强制 probe → 写凭据 → 发布目录 → 启动 runtime。
    pub async fn create_account(
        &self,
        draft: AccountDraft,
        credentials: AccountCredentials,
    ) -> Result<(AccountProfile, bool), ManagerError> {
        self.reject_if_shutting_down()?;
        self.validate_draft(&draft, &credentials)?;
        let _permit = self.acquire_permit().await?;
        let _serial = self.mutation_lock.lock().await;

        {
            let state = self.state.lock().unwrap();
            let unarchived = state
                .catalog
                .profiles
                .iter()
                .filter(|profile| profile.archived_at.is_none())
                .count();
            if unarchived >= account_catalog::MAX_UNARCHIVED_PROFILES {
                return Err(ManagerError::Conflict(format!(
                    "未归档账户已达上限 {}",
                    account_catalog::MAX_UNARCHIVED_PROFILES
                )));
            }
            ensure_plugin_mailbox_binding_available(
                &state.catalog,
                &draft.address,
                &draft.incoming,
                draft.outgoing.as_ref(),
                None,
            )?;
        }

        // 1. 强制 probe（用提交的秘密直连，不落盘；委托方向走 provider）。
        let outgoing_secret = self.draft_outgoing_secret(&draft, &credentials)?;
        let report = self
            .draft_probe_report("", &draft, &credentials.incoming, outgoing_secret.as_deref())
            .await;
        ensure_probe_ok(&report)?;

        // 2. 组装 profile 并以 revision 1 写入凭据。
        let id = loop {
            let candidate = account_catalog::generate_account_id();
            let taken = {
                let state = self.state.lock().unwrap();
                state.catalog.profile(&candidate).is_some()
            };
            if !taken {
                break candidate;
            }
        };
        let delegated_incoming = matches!(draft.incoming, IncomingSettings::Plugin { .. });
        let mut profile = AccountProfile {
            id: id.clone(),
            revision: 1,
            display_name: draft.display_name.clone(),
            address: draft.address.clone(),
            enabled: true,
            archived_at: None,
            incoming_credential_revision: if delegated_incoming { None } else { Some(1) },
            outgoing_credential_revision: None,
            incoming: draft.incoming.clone(),
            outgoing: draft.outgoing.clone(),
        };
        let mut staged: Vec<CredentialKey> = Vec::new();
        if !delegated_incoming {
            let incoming_key = CredentialKey::new(id.clone(), CredentialRole::Incoming, 1);
            self.store
                .save(&incoming_key, &credentials.incoming)
                .map_err(ManagerError::Credential)?;
            staged.push(incoming_key);
        }
        if let Some(outgoing) = &draft.outgoing {
            if matches!(outgoing.smtp(), Some((_, _, _, _, CredentialSharing::Separate))) {
                let secret = credentials
                    .outgoing
                    .as_deref()
                    .ok_or_else(|| ManagerError::Invalid("separate 发信凭据缺失".into()))?;
                let key = CredentialKey::new(id.clone(), CredentialRole::Outgoing, 1);
                self.store
                    .save(&key, secret)
                    .map_err(|error| {
                        for key in &staged {
                            let _ = self.store.delete(key);
                        }
                        ManagerError::Credential(error)
                    })?;
                staged.push(key.clone());
                profile.outgoing_credential_revision = Some(1);
            }
        }

        // 3. 原子发布目录。
        let previous_catalog = {
            let result = self
                .publish_with(|catalog| catalog.profiles.push(profile.clone()))
                .await;
            if result.is_err() {
                for key in &staged {
                    let _ = self.store.delete(key);
                }
            }
            result?
        };

        // 4. 启动 runtime；内部失败则恢复上一版文件与凭据引用。
        if let Err(error) = self.mount_runtime(&profile) {
            let _ = self.restore_catalog(previous_catalog);
            for key in &staged {
                let _ = self.store.delete(key);
            }
            return Err(error);
        }
        let active = profile.enabled;
        Ok((profile, active))
    }

    // —— update ——

    /// updateAccount：revision 冲突检查 → 按 patch 重组 → 必要时 probe →
    /// stage 新凭据 → 发布 → 重挂载 runtime → 成功后删除被替换的旧凭据。
    pub async fn update_account(
        &self,
        id: &str,
        expected_revision: u64,
        patch: AccountPatch,
        credentials: Option<AccountCredentials>,
    ) -> Result<ProfileView, ManagerError> {
        self.reject_if_shutting_down()?;
        let _permit = self.acquire_permit().await?;
        let _serial = self.mutation_lock.lock().await;

        let current = {
            let state = self.state.lock().unwrap();
            let profile = state.catalog.profile(id).ok_or(ManagerError::NotFound)?;
            if profile.archived_at.is_some() {
                return Err(ManagerError::NotFound);
            }
            if profile.revision != expected_revision {
                return Err(ManagerError::Conflict(format!(
                    "revision 期望 {expected_revision}，实际 {}",
                    profile.revision
                )));
            }
            profile.clone()
        };

        let mut next = current.clone();
        let address_changed = patch
            .address
            .as_ref()
            .is_some_and(|address| address != &current.address);
        if let Some(display_name) = &patch.display_name {
            next.display_name = display_name.clone();
        }
        if let Some(address) = &patch.address {
            next.address = address.clone();
        }
        let incoming_changed = match &patch.incoming {
            Some(incoming) => {
                let changed = *incoming != next.incoming;
                next.incoming = incoming.clone();
                changed
            }
            None => false,
        };
        let mut replaced_keys: Vec<CredentialKey> = Vec::new();
        let outgoing_changed = match &patch.outgoing {
            None => false,
            Some(None) => {
                if let Some(revision) = next.outgoing_credential_revision.take() {
                    replaced_keys.push(CredentialKey::new(
                        id.to_string(),
                        CredentialRole::Outgoing,
                        revision,
                    ));
                }
                next.outgoing = None;
                true
            }
            Some(Some(outgoing)) => {
                let changed = Some(outgoing) != next.outgoing.as_ref();
                if changed {
                    if let Some(revision) = next.outgoing_credential_revision.take() {
                        replaced_keys.push(CredentialKey::new(
                            id.to_string(),
                            CredentialRole::Outgoing,
                            revision,
                        ));
                    }
                }
                next.outgoing = Some(outgoing.clone());
                changed
            }
        };
        let new_incoming_secret = credentials
            .as_ref()
            .map(|credentials| !credentials.incoming.is_empty())
            .unwrap_or(false);
        let new_outgoing_secret = credentials
            .as_ref()
            .and_then(|credentials| credentials.outgoing.as_deref())
            .is_some();

        // 委托方向（ADR-0021）：不接受秘密；切换为委托时释放既有凭据引用。
        if matches!(next.incoming, IncomingSettings::Plugin { .. }) {
            if new_incoming_secret {
                return Err(ManagerError::Invalid(
                    "incoming 委托给插件时不接受凭据".into(),
                ));
            }
            if incoming_changed {
                if let Some(revision) = next.incoming_credential_revision.take() {
                    replaced_keys.push(CredentialKey::new(
                        id.to_string(),
                        CredentialRole::Incoming,
                        revision,
                    ));
                }
            }
        }
        if matches!(&next.outgoing, Some(OutgoingSettings::Plugin { .. })) {
            if new_outgoing_secret {
                return Err(ManagerError::Invalid(
                    "outgoing 委托给插件时不接受凭据".into(),
                ));
            }
            if outgoing_changed {
                if let Some(revision) = next.outgoing_credential_revision.take() {
                    replaced_keys.push(CredentialKey::new(
                        id.to_string(),
                        CredentialRole::Outgoing,
                        revision,
                    ));
                }
            }
        }

        account_catalog::validate_profile(&next).map_err(ManagerError::Invalid)?;
        {
            let state = self.state.lock().unwrap();
            ensure_plugin_mailbox_binding_available(
                &state.catalog,
                &next.address,
                &next.incoming,
                next.outgoing.as_ref(),
                Some(id),
            )?;
        }
        if let Some(credentials) = &credentials {
            if new_incoming_secret {
                validate_secret(&credentials.incoming)?;
            }
            if new_outgoing_secret {
                validate_secret(credentials.outgoing.as_deref().unwrap_or_default())?;
            }
        }

        // API provider 用规范 address 定位远端 mailbox；它变化时必须重新 probe。
        // incoming provider 的轮询 spec 也持有 address，因此复用既有重挂载事务，
        // 避免目录已更新、runtime 仍轮询旧邮箱的分裂状态。
        let provider_address_changed = address_changed
            && (matches!(next.incoming, IncomingSettings::Plugin { .. })
                || matches!(&next.outgoing, Some(OutgoingSettings::Plugin { .. })));
        let connection_changed = incoming_changed
            || outgoing_changed
            || new_incoming_secret
            || new_outgoing_secret
            || provider_address_changed;

        // 需要时 probe：endpoint、用户名或秘密变化才强制；纯展示字段编辑不打扰服务器。
        if connection_changed {
            let (incoming_secret, outgoing_secret) = self
                .effective_probe_secrets(&next, credentials.as_ref())
                .await?;
            let draft = AccountDraft {
                display_name: next.display_name.clone(),
                address: next.address.clone(),
                incoming: next.incoming.clone(),
                outgoing: next.outgoing.clone(),
            };
            let report =
                self.draft_probe_report(
                    id,
                    &draft,
                    &incoming_secret,
                    outgoing_secret.as_deref(),
                )
                .await;
            ensure_probe_ok(&report)?;
        }

        // stage 新凭据（revision 单调递增）。
        let mut staged: Vec<CredentialKey> = Vec::new();
        if new_incoming_secret {
            let revision = next.incoming_credential_revision.unwrap_or(0) + 1;
            let key = CredentialKey::new(id.to_string(), CredentialRole::Incoming, revision);
            let secret = credentials.as_ref().unwrap().incoming.clone();
            self.stage_credential(&key, &secret, &staged)?;
            staged.push(key);
            next.incoming_credential_revision = Some(revision);
        }
        if let Some(outgoing) = &next.outgoing {
            if matches!(outgoing.smtp(), Some((_, _, _, _, CredentialSharing::Separate)))
                && new_outgoing_secret
            {
                let revision = next.outgoing_credential_revision.unwrap_or(0) + 1;
                let key = CredentialKey::new(id.to_string(), CredentialRole::Outgoing, revision);
                let secret = credentials
                    .as_ref()
                    .unwrap()
                    .outgoing
                    .clone()
                    .ok_or_else(|| ManagerError::Invalid("separate 发信凭据缺失".into()))?;
                self.stage_credential(&key, &secret, &staged)?;
                staged.push(key);
                next.outgoing_credential_revision = Some(revision);
            }
        }
        // 复用收件密码的 outgoing 不保留独立 revision 引用。
        if let Some(outgoing) = &next.outgoing {
            if matches!(outgoing.smtp(), Some((_, _, _, _, CredentialSharing::Incoming))) {
                next.outgoing_credential_revision = None;
            }
        }

        next.revision = current.revision + 1;

        // 发布目录；失败则清理 staged 并保持旧文件/旧运行时。
        let previous_catalog = {
            let result = self.publish_with(|catalog| {
                if let Some(slot) = catalog.profile_mut(id) {
                    *slot = next.clone();
                }
            })
            .await;
            if result.is_err() {
                for key in &staged {
                    let _ = self.store.delete(key);
                }
            }
            result?
        };

        // 重挂载 runtime：endpoint/username/secret 变化必须先停止旧 realm 再挂新 realm，
        // 避免两个 IDLE 连接并存。
        if connection_changed {
            if let Some(old) = self.take_runtime(id) {
                Self::stop_runtime(old).await;
                self.accounts.remove(id);
            }
            if let Err(error) = self.mount_runtime(&next) {
                // 运行时构造失败：恢复上一版文件和凭据引用，并按旧配置重启 runtime。
                let _ = self.restore_catalog(previous_catalog);
                for key in &staged {
                    let _ = self.store.delete(key);
                }
                let restored = current.clone();
                let _ = self.mount_runtime(&restored);
                return Err(error);
            }
        }

        // 成功后删除被替换的旧 revision。
        for key in &replaced_keys {
            let _ = self.store.delete(key);
        }

        Ok(self
            .profiles_view(true)
            .into_iter()
            .find(|view| view.id == id)
            .expect("刚更新的 profile 必然在视图中"))
    }

    async fn effective_probe_secrets(
        &self,
        next: &AccountProfile,
        credentials: Option<&AccountCredentials>,
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), ManagerError> {
        let incoming_secret = match &next.incoming {
            // 委托方向无凭据语义：探测秘密留空（provider 探测不使用它）。
            IncomingSettings::Plugin { .. } => Vec::new(),
            IncomingSettings::Imap { .. } => match credentials {
                Some(credentials) if !credentials.incoming.is_empty() => {
                    credentials.incoming.clone()
                }
                _ => {
                    let revision = next
                        .incoming_credential_revision
                        .ok_or_else(|| ManagerError::Invalid("账户缺少收信凭据引用".into()))?;
                    self.store
                        .load(&CredentialKey::new(
                            next.id.clone(),
                            CredentialRole::Incoming,
                            revision,
                        ))
                        .map_err(ManagerError::Credential)?
                        .expose()
                        .to_vec()
                }
            },
        };
        let outgoing_secret = match &next.outgoing {
            None => None,
            // 委托方向无凭据。
            Some(OutgoingSettings::Plugin { .. }) => None,
            Some(outgoing) => match outgoing.smtp().expect("Smtp 变体已匹配").4 {
                CredentialSharing::Incoming => Some(incoming_secret.clone()),
                CredentialSharing::Separate => match credentials {
                    Some(credentials) if !credentials.outgoing.as_deref().unwrap_or_default().is_empty() => {
                        credentials.outgoing.clone()
                    }
                    _ => match next.outgoing_credential_revision {
                        Some(revision) => Some(
                            self.store
                                .load(&CredentialKey::new(
                                    next.id.clone(),
                                    CredentialRole::Outgoing,
                                    revision,
                                ))
                                .map_err(ManagerError::Credential)?
                                .expose()
                                .to_vec(),
                        ),
                        None => None,
                    },
                },
            },
        };
        Ok((incoming_secret, outgoing_secret))
    }

    fn stage_credential(
        &self,
        key: &CredentialKey,
        secret: &[u8],
        staged: &[CredentialKey],
    ) -> Result<(), ManagerError> {
        self.store.save(key, secret).map_err(|error| {
            for key in staged {
                let _ = self.store.delete(key);
            }
            ManagerError::Credential(error)
        })
    }

    // —— enable / disable ——

    /// setAccountEnabled：enable 挂载 runtime；disable 先停 realm 再发布最终状态。
    pub async fn set_account_enabled(
        &self,
        id: &str,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<ProfileView, ManagerError> {
        self.reject_if_shutting_down()?;
        let _permit = self.acquire_permit().await?;
        let _serial = self.mutation_lock.lock().await;

        let current = {
            let state = self.state.lock().unwrap();
            let profile = state.catalog.profile(id).ok_or(ManagerError::NotFound)?;
            if profile.archived_at.is_some() {
                return Err(ManagerError::NotFound);
            }
            if profile.revision != expected_revision {
                return Err(ManagerError::Conflict(format!(
                    "revision 期望 {expected_revision}，实际 {}",
                    profile.revision
                )));
            }
            profile.clone()
        };
        if current.enabled == enabled {
            return Ok(self
                .profiles_view(true)
                .into_iter()
                .find(|view| view.id == id)
                .expect("存在的 profile 必然在视图中"));
        }

        let mut next = current.clone();
        next.enabled = enabled;
        next.revision = current.revision + 1;

        let previous_catalog = self
            .publish_with(|catalog| {
                if let Some(slot) = catalog.profile_mut(id) {
                    *slot = next.clone();
                }
            })
            .await?;

        if enabled {
            if let Err(error) = self.mount_runtime(&next) {
                let _ = self.restore_catalog(previous_catalog);
                return Err(error);
            }
        } else if let Some(runtime) = self.take_runtime(id) {
            Self::stop_runtime(runtime).await;
            self.accounts.remove(id);
        }

        Ok(self
            .profiles_view(true)
            .into_iter()
            .find(|view| view.id == id)
            .expect("刚更新的 profile 必然在视图中"))
    }

    // —— remove（软归档） ——

    /// removeAccount：停运行时 → 删凭据 → 归档发布。本地邮件一律保留。
    pub async fn remove_account(
        &self,
        id: &str,
        expected_revision: u64,
    ) -> Result<RemoveOutcome, ManagerError> {
        self.reject_if_shutting_down()?;
        let _permit = self.acquire_permit().await?;
        let _serial = self.mutation_lock.lock().await;

        let current = {
            let state = self.state.lock().unwrap();
            let profile = state.catalog.profile(id).ok_or(ManagerError::NotFound)?;
            profile.clone()
        };

        // 若上一次调用已发布归档、但地址 DELETE 因瞬时 SQLite 错误失败，
        // 或进程恰好在二者之间退出，允许后续 remove 重试完成级联；
        // 没有残留时仍保持 NotFound 契约。归档记录已经不可再变更，
        // 因而这里不以旧 expected_revision 阻断幂等清理。
        if current.archived_at.is_some() {
            if let Some(store) = self.ctx.get::<crate::store::Store>() {
                let removed = store.delete_addresses_for_account(id).map_err(|error| {
                    ManagerError::Internal(format!("归档账户地址重试清理失败: {error}"))
                })?;
                if removed > 0 {
                    eprintln!(
                        "[maild/accounts] 归档账户地址重试清理 account={} removed={removed}",
                        id
                    );
                    return Ok(RemoveOutcome {
                        archived: true,
                        // 归档记录已清空 credential revision，无法证明首次调用
                        // 在发布前是否已成功删除 Keychain item；保守报告 false。
                        credentials_deleted: false,
                        messages_retained: true,
                    });
                }
            }
            return Err(ManagerError::NotFound);
        }

        if current.revision != expected_revision {
            return Err(ManagerError::Conflict(format!(
                "revision 期望 {expected_revision}，实际 {}",
                current.revision
            )));
        }

        if let Some(runtime) = self.take_runtime(id) {
            Self::stop_runtime(runtime).await;
            self.accounts.remove(id);
        }

        let mut deleted = true;
        if let Some(revision) = current.incoming_credential_revision {
            deleted &= self
                .store
                .delete(&CredentialKey::new(id.to_string(), CredentialRole::Incoming, revision))
                .unwrap_or(false);
        }
        if let Some(revision) = current.outgoing_credential_revision {
            deleted &= self
                .store
                .delete(&CredentialKey::new(id.to_string(), CredentialRole::Outgoing, revision))
                .unwrap_or(false);
        }

        let mut next = current.clone();
        next.enabled = false;
        next.archived_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
        );
        next.incoming_credential_revision = None;
        next.outgoing_credential_revision = None;
        next.revision = current.revision + 1;

        self.publish_with(|catalog| {
            if let Some(slot) = catalog.profile_mut(id) {
                *slot = next;
            }
        })
        .await?;

        // 地址是账户的从属资源；邮件 recipient 只是历史文本，不随账户删除。
        // 目录发布后清理，避免发布失败时先丢地址。Store 未装配意味着本进程
        // 不存在地址层数据（精简测试上下文兼容）。
        if let Some(store) = self.ctx.get::<crate::store::Store>() {
            let removed = store
                .delete_addresses_for_account(id)
                .map_err(|error| ManagerError::Internal(format!("账户地址清理失败: {error}")))?;
            eprintln!(
                "[maild/accounts] 账户地址清理 account={} removed={removed}",
                id
            );
        }

        Ok(RemoveOutcome {
            archived: true,
            credentials_deleted: deleted,
            messages_retained: true,
        })
    }

    // —— per-account 发信 ——

    /// sendFromAccount：从 profile 固定选择发件地址、SMTP endpoint 和凭据，
    /// 不允许请求伪造 From。无 outgoing 或已停用的账户拒绝发信。
    pub async fn send_from_account(
        &self,
        account: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> Result<(), ManagerError> {
        self.send_from_account_with_address(account, to, subject, body, None)
            .await
    }

    /// ADR-0022 additive 入口：在既有账户发信语义上选择一个 active 别名。
    pub async fn send_from_account_with_address(
        &self,
        account: &str,
        to: &str,
        subject: &str,
        body: &str,
        from_address: Option<&str>,
    ) -> Result<(), ManagerError> {
        self.reject_if_shutting_down()?;
        let _permit = self.acquire_permit().await?;

        let profile = {
            let state = self.state.lock().unwrap();
            let profile = state
                .catalog
                .profile(account)
                .ok_or(ManagerError::NotFound)?;
            if profile.archived_at.is_some() {
                return Err(ManagerError::NotFound);
            }
            profile.clone()
        };
        if !profile.enabled {
            return Err(ManagerError::Invalid("账户已停用，不能发信".into()));
        }
        // ADR-0022：可选别名发信。只重写 From——凭据、连接、规则语义、
        // 审计纪律全部沿用父账户，不可能绕过任何账户级纪律。
        let sender = self.resolve_send_from(&profile, from_address)?;
        let outgoing = profile
            .outgoing
            .clone()
            .ok_or_else(|| ManagerError::Invalid("账户未配置发信（SMTP）".into()))?;

        // 委托发信（M4-05，ADR-0021）：反向调用 provider；凭据不跨缝，
        // from 为 profile.address 或经校验的别名。永不静默回退内置 SMTP。
        if let OutgoingSettings::Plugin { provider } = &outgoing {
            crate::transport::validate_message(&sender, to, subject, body)
                .map_err(ManagerError::Invalid)?;
            return self
                .send_via_provider(&profile, provider, &sender, to, subject, body)
                .await;
        }

        let (server, port, security, username, sharing) =
            outgoing.smtp().expect("Smtp 变体已匹配");
        let secret_bytes = match sharing {
            CredentialSharing::Incoming => {
                let revision = profile
                    .incoming_credential_revision
                    .ok_or_else(|| ManagerError::Invalid("账户缺少收信凭据引用".into()))?;
                self.store
                    .load(&CredentialKey::new(
                        profile.id.clone(),
                        CredentialRole::Incoming,
                        revision,
                    ))
                    .map_err(ManagerError::Credential)?
                    .expose()
                    .to_vec()
            }
            CredentialSharing::Separate => {
                let revision = profile
                    .outgoing_credential_revision
                    .ok_or_else(|| ManagerError::Invalid("账户缺少发信凭据引用".into()))?;
                self.store
                    .load(&CredentialKey::new(
                        profile.id.clone(),
                        CredentialRole::Outgoing,
                        revision,
                    ))
                    .map_err(ManagerError::Credential)?
                    .expose()
                    .to_vec()
            }
        };
        let password = String::from_utf8(secret_bytes)
            .map_err(|_| ManagerError::Invalid("发信密码不是有效的 UTF-8 文本".into()))?;

        crate::transport::validate_message(&sender, to, subject, body)
            .map_err(ManagerError::Invalid)?;

        let config = TransportConfig {
            server: server.to_string(),
            port,
            username: username.to_string(),
            password,
            tls: security.as_str().to_string(),
            accept_invalid_certs: false,
        };
        crate::transport::Transport::from_config(config)
            .send(&sender, to, subject, body)
            .await
            .map_err(|error| ManagerError::Internal(format!("发送失败: {error}")))
    }

    /// 富消息发信（ADR-0028）。`draft` 的 `from` 与 `message_id` 由这里覆盖——
    /// 发件地址要经过别名校验，`Message-ID` 的域名必须与最终 From 一致。
    ///
    /// 返回**投递出去的那一份报文**，调用方用它归档到已发送。
    pub async fn send_message_from_account(
        &self,
        account: &str,
        draft: &crate::compose::OutgoingMessage,
        from_address: Option<&str>,
    ) -> Result<String, ManagerError> {
        self.reject_if_shutting_down()?;
        let _permit = self.acquire_permit().await?;

        let profile = {
            let state = self.state.lock().unwrap();
            let profile = state
                .catalog
                .profile(account)
                .ok_or(ManagerError::NotFound)?;
            if profile.archived_at.is_some() {
                return Err(ManagerError::NotFound);
            }
            profile.clone()
        };
        if !profile.enabled {
            return Err(ManagerError::Invalid("账户已停用，不能发信".into()));
        }
        let sender = self.resolve_send_from(&profile, from_address)?;
        let outgoing = profile
            .outgoing
            .clone()
            .ok_or_else(|| ManagerError::Invalid("账户未配置发信（SMTP）".into()))?;

        let mut message = draft.clone();
        message.from = sender.clone();
        message.message_id = crate::compose::new_message_id(&sender, message.date_unix);

        if let OutgoingSettings::Plugin { provider } = &outgoing {
            // ADR-0028 决策 10：委托合同是纯文本单收件人。明确拒绝，不静默降级。
            if message.to.len() != 1
                || !message.cc.is_empty()
                || !message.bcc.is_empty()
                || !message.body_html.is_empty()
                || !message.attachments.is_empty()
            {
                return Err(ManagerError::Invalid(
                    "委托 provider 的发信合同只支持单收件人纯文本（不支持 Cc/Bcc/HTML/附件）"
                        .into(),
                ));
            }
            let to = message.to[0].clone();
            crate::transport::validate_message(&sender, &to, &message.subject, &message.body)
                .map_err(ManagerError::Invalid)?;
            self.send_via_provider(&profile, provider, &sender, &to, &message.subject, &message.body)
                .await?;
            return Ok(crate::compose::assemble(&message));
        }

        let (server, port, security, username, sharing) =
            outgoing.smtp().expect("Smtp 变体已匹配");
        let secret_bytes = self.outgoing_secret(&profile, sharing)?;
        let password = String::from_utf8(secret_bytes)
            .map_err(|_| ManagerError::Invalid("发信密码不是有效的 UTF-8 文本".into()))?;

        let config = TransportConfig {
            server: server.to_string(),
            port,
            username: username.to_string(),
            password,
            tls: security.as_str().to_string(),
            accept_invalid_certs: false,
        };
        crate::transport::Transport::from_config(config)
            .send_message_checked(&message)
            .await
            .map_err(ManagerError::Delivery)
    }

    fn outgoing_secret(
        &self,
        profile: &AccountProfile,
        sharing: CredentialSharing,
    ) -> Result<Vec<u8>, ManagerError> {
        let (role, revision) = match sharing {
            CredentialSharing::Incoming => (
                CredentialRole::Incoming,
                profile
                    .incoming_credential_revision
                    .ok_or_else(|| ManagerError::Invalid("账户缺少收信凭据引用".into()))?,
            ),
            CredentialSharing::Separate => (
                CredentialRole::Outgoing,
                profile
                    .outgoing_credential_revision
                    .ok_or_else(|| ManagerError::Invalid("账户缺少发信凭据引用".into()))?,
            ),
        };
        Ok(self
            .store
            .load(&CredentialKey::new(profile.id.clone(), role, revision))
            .map_err(ManagerError::Credential)?
            .expose()
            .to_vec())
    }

    /// 这个账户存不存在（受管 profile 或 legacy 配置项）。
    /// 草稿创建用它兜一道：写一份发不出去的草稿没有意义（ADR-0034）。
    pub fn has_account(&self, account: &str) -> bool {
        let state = self.state.lock().unwrap();
        state.catalog.profile(account).is_some()
            || state.legacy.iter().any(|(name, ..)| name == account)
    }

    /// 账户自身的地址集合：profile 地址 + 名下 active 别名。
    /// 「全部回复」用它排除自己（ADR-0028 决策 7）。
    pub fn own_addresses(&self, account: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        {
            let state = self.state.lock().unwrap();
            if let Some(profile) = state.catalog.profile(account) {
                out.push(profile.address.clone());
            }
        }
        if let Ok(addresses) = self.list_addresses(Some(account), false) {
            out.extend(addresses.into_iter().map(|view| view.address));
        }
        out
    }

    /// 解析发信 From：省略或显式主地址均使用 profile.address；其他地址必须是
    /// 本账户名下 active（含未过期）的地址行。审计只记地址行 id，不记地址串。
    fn resolve_send_from(
        &self,
        profile: &AccountProfile,
        from_address: Option<&str>,
    ) -> Result<String, ManagerError> {
        let Some(requested) = from_address else {
            return Ok(profile.address.clone());
        };
        if requested == profile.address {
            return Ok(profile.address.clone());
        }
        let store = self.address_store()?;
        store
            .expire_due_addresses(unix_now())
            .map_err(|error| ManagerError::Internal(format!("地址过期判定失败: {error}")))?;
        let records = store
            .list_addresses(Some(&profile.id))
            .map_err(|error| ManagerError::Internal(format!("地址读取失败: {error}")))?;
        let record = records
            .iter()
            .find(|record| record.address == requested)
            .ok_or_else(|| {
                ManagerError::Invalid("发信地址不存在或不属于该账户".to_string())
            })?;
        if record.effective_state(unix_now()) != crate::addresses::AddressState::Active {
            return Err(ManagerError::Invalid(
                "发信地址已停用或已过期".into(),
            ));
        }
        eprintln!(
            "[maild/accounts] alias 发信 account={} address_id={}",
            profile.id, record.id
        );
        Ok(record.address.clone())
    }

    /// 经 provider 插件发信。审计行只含 principal 维度缺失（用户动作经 RPC
    /// 触发）与结果，不含收件人/主题/正文。
    async fn send_via_provider(
        &self,
        profile: &AccountProfile,
        provider_id: &str,
        sender: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> Result<(), ManagerError> {
        let unavailable = |code: i64, detail: String| ManagerError::Provider(code, detail);
        let Some(registry) = self.ctx.get::<crate::provider::ProviderRegistry>() else {
            return Err(unavailable(
                crate::provider::PROVIDER_UNAVAILABLE,
                "provider 注册表未装配".into(),
            ));
        };
        let Some(endpoint) = registry.endpoint_of(provider_id) else {
            return Err(unavailable(
                crate::provider::PROVIDER_UNAVAILABLE,
                format!("provider {provider_id} 未注册"),
            ));
        };
        let outcome = endpoint
            .call(
                "transport/send",
                serde_json::to_value(TransportSendRequest {
                    account: profile.id.clone(),
                    account_address: profile.address.clone(),
                    from: sender.to_owned(),
                    to: to.to_owned(),
                    subject: subject.to_owned(),
                    body: body.to_owned(),
                })
                .expect("transport send DTO is serializable"),
                crate::provider::send_budget(),
            )
            .await;
        match outcome {
            Ok(_) => {
                eprintln!(
                    "[maild/provider] 委托发信 account={} provider={} outcome=ok",
                    profile.id, provider_id
                );
                Ok(())
            }
            Err(crate::provider::ProviderCallError::Timeout) => {
                eprintln!(
                    "[maild/provider] 委托发信 account={} provider={} outcome=timeout",
                    profile.id, provider_id
                );
                Err(unavailable(
                    crate::provider::PROVIDER_TIMEOUT,
                    "provider 发信响应超时".into(),
                ))
            }
            Err(crate::provider::ProviderCallError::Unavailable(detail)) => {
                eprintln!(
                    "[maild/provider] 委托发信 account={} provider={} outcome=unavailable",
                    profile.id, provider_id
                );
                Err(unavailable(crate::provider::PROVIDER_UNAVAILABLE, detail))
            }
            Err(crate::provider::ProviderCallError::Upstream { code, message }) => {
                let detail = crate::probe::sanitize_detail(&message);
                eprintln!(
                    "[maild/provider] 委托发信 account={} provider={} outcome=error code={code}",
                    profile.id, provider_id
                );
                Err(unavailable(crate::provider::PROVIDER_ERROR, detail))
            }
        }
    }

    // —— 地址层（M4-06，ADR-0022） ——

    fn address_store(&self) -> Result<Arc<crate::store::Store>, ManagerError> {
        self.ctx
            .get::<crate::store::Store>()
            .ok_or_else(|| ManagerError::Internal("store 服务未装配".into()))
    }

    pub fn list_addresses(
        &self,
        account: Option<&str>,
        include_inactive: bool,
    ) -> Result<Vec<crate::addresses::AddressView>, ManagerError> {
        self.reject_if_shutting_down()?;
        if let Some(account) = account {
            self.require_live_profile(account)?;
        }
        let store = self.address_store()?;
        store
            .expire_due_addresses(unix_now())
            .map_err(|error| ManagerError::Internal(format!("地址过期判定失败: {error}")))?;
        let records = store
            .list_addresses(account)
            .map_err(|error| ManagerError::Internal(format!("地址读取失败: {error}")))?;
        Ok(records
            .into_iter()
            .filter(|record| {
                include_inactive || record.effective_state(unix_now()) == crate::addresses::AddressState::Active
            })
            .map(|record| record.into_view())
            .collect())
    }

    /// 创建别名或一次性地址。disposable 可省略 address（daemon 生成）。
    pub fn create_address(
        &self,
        account: &str,
        kind: &str,
        address: Option<&str>,
        label: Option<&str>,
        expires_secs: Option<u64>,
    ) -> Result<crate::addresses::AddressView, ManagerError> {
        self.reject_if_shutting_down()?;
        let kind = crate::addresses::AddressKind::parse(kind).ok_or_else(|| {
            ManagerError::Invalid("kind 必须是 alias 或 disposable".into())
        })?;
        if kind == crate::addresses::AddressKind::Alias && expires_secs.is_some() {
            return Err(ManagerError::Invalid(
                "expires_secs 仅适用于 disposable 地址".into(),
            ));
        }
        let label = match label {
            Some(label) => validate_address_label(label).map_err(ManagerError::Invalid)?,
            None => String::new(),
        };
        let expires_at = match expires_secs {
            None => None,
            Some(secs) => {
                if secs == 0 || secs > MAX_ADDRESS_TTL_SECS {
                    return Err(ManagerError::Invalid(format!(
                        "expires_secs 必须在 1..={MAX_ADDRESS_TTL_SECS}"
                    )));
                }
                Some(unix_now().saturating_add(secs))
            }
        };

        let profile = self.require_live_profile(account)?;
        let store = self.address_store()?;

        // 显式地址（两种 kind 都允许显式指定）；None 仅对 disposable 表示
        // 由 daemon 随机生成——冲突时换一个再试；显式地址唯一冲突直接报错。
        let explicit = address
            .map(|value| -> Result<String, ManagerError> {
                let value = value.trim();
                crate::addresses::validate_address(value).map_err(ManagerError::Invalid)?;
                Ok(value.to_owned())
            })
            .transpose()?;
        let domain = match kind {
            crate::addresses::AddressKind::Disposable if explicit.is_none() => Some(
                crate::addresses::domain_of(&profile.address)
                    .ok_or_else(|| ManagerError::Internal("账户地址缺少域名".into()))?
                    .to_owned(),
            ),
            _ => None,
        };
        let attempts = if domain.is_some() {
            DISPOSABLE_GENERATE_RETRIES
        } else {
            1
        };
        for _ in 0..attempts {
            let resolved = match (&explicit, &domain) {
                (Some(address), _) => address.clone(),
                (None, Some(domain)) => format!(
                    "{}@{domain}",
                    crate::addresses::generate_disposable_local_part()
                ),
                (None, None) => {
                    return Err(ManagerError::Invalid("alias 必须显式提供 address".into()));
                }
            };
            let record = crate::addresses::AddressRecord {
                id: crate::addresses::generate_address_id(),
                account: profile.id.clone(),
                address: resolved,
                kind,
                label: label.clone(),
                state: crate::addresses::AddressState::Active,
                created_at: unix_now(),
                expires_at,
            };
            match store.insert_address_bounded(
                &record,
                crate::addresses::MAX_ADDRESSES_PER_ACCOUNT,
            ) {
                Ok(crate::store::InsertAddressOutcome::Inserted) => {
                    // createAddress 与 removeAccount 走不同的同步/异步入口；
                    // 插入后复核父账户，覆盖“删除已发布并清理后才插入”的交错。
                    if let Err(profile_error) = self.require_live_profile(&profile.id) {
                        store.delete_address(&record.id).map_err(|error| {
                            ManagerError::Internal(format!("孤儿地址回收失败: {error}"))
                        })?;
                        return Err(profile_error);
                    }
                    eprintln!(
                        "[maild/accounts] 地址创建 account={} address_id={} kind={}",
                        profile.id,
                        record.id,
                        kind.as_str()
                    );
                    return Ok(record.into_view());
                }
                Ok(crate::store::InsertAddressOutcome::Duplicate) if domain.is_some() => continue,
                Ok(crate::store::InsertAddressOutcome::Duplicate) => {
                    return Err(ManagerError::Conflict("地址已存在".into()));
                }
                Ok(crate::store::InsertAddressOutcome::LimitReached) => {
                    return Err(ManagerError::Conflict(format!(
                        "该账户地址数已达上限 {}",
                        crate::addresses::MAX_ADDRESSES_PER_ACCOUNT
                    )));
                }
                Err(error) => {
                    return Err(ManagerError::Internal(format!("地址写入失败: {error}")));
                }
            }
        }
        Err(ManagerError::Conflict(
            "随机地址生成连续冲突，请重试".into(),
        ))
    }

    /// 更新状态/标签。expired 是派生态，不接受显式设置。
    pub fn update_address(
        &self,
        id: &str,
        state: Option<&str>,
        label: Option<&str>,
    ) -> Result<crate::addresses::AddressView, ManagerError> {
        self.reject_if_shutting_down()?;
        let new_state = match state {
            None => None,
            Some(value) => match crate::addresses::AddressState::parse(value) {
                Some(parsed @ (crate::addresses::AddressState::Active
                | crate::addresses::AddressState::Disabled)) => Some(parsed),
                _ => {
                    return Err(ManagerError::Invalid(
                        "state 只能是 active 或 disabled".into(),
                    ));
                }
            },
        };
        let new_label = match label {
            Some(label) => Some(validate_address_label(label).map_err(ManagerError::Invalid)?),
            None => None,
        };
        let store = self.address_store()?;
        let updated = match store
            .patch_address(id, new_state, new_label.as_deref(), unix_now())
            .map_err(|error| ManagerError::Internal(format!("地址更新失败: {error}")))?
        {
            crate::store::PatchAddressOutcome::Updated(record) => record,
            crate::store::PatchAddressOutcome::NotFound => return Err(ManagerError::NotFound),
            crate::store::PatchAddressOutcome::Expired => {
                return Err(ManagerError::Invalid(
                    "地址已过期，不能重新启用或停用".into(),
                ));
            }
        };
        eprintln!(
            "[maild/accounts] 地址更新 address_id={} state={}",
            id,
            updated.state.as_str()
        );
        Ok(updated.into_view())
    }

    /// 删除地址行；消息里的 recipient 成为悬垂文本（历史保留）。
    pub fn remove_address(&self, id: &str) -> Result<(), ManagerError> {
        self.reject_if_shutting_down()?;
        let store = self.address_store()?;
        let deleted = store
            .delete_address(id)
            .map_err(|error| ManagerError::Internal(format!("地址删除失败: {error}")))?;
        if !deleted {
            return Err(ManagerError::NotFound);
        }
        eprintln!("[maild/accounts] 地址删除 address_id={id}");
        Ok(())
    }

    /// 账户存在且未归档（地址族操作的前置）；返回 owned 快照。
    fn require_live_profile(&self, account: &str) -> Result<AccountProfile, ManagerError> {
        let state = self.state.lock().unwrap();
        let profile = state
            .catalog
            .profile(account)
            .ok_or(ManagerError::NotFound)?;
        if profile.archived_at.is_some() {
            return Err(ManagerError::NotFound);
        }
        Ok(profile.clone())
    }
}

/// unix 秒（时钟回退时取 0；地址 TTL 只要求单调近似）。
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// 地址标签上限（字节），无控制字符。
const MAX_ADDRESS_LABEL_BYTES: usize = 120;
/// disposable 随机生成唯一冲突的重试次数（60 bit 熵下概率不可达）。
const DISPOSABLE_GENERATE_RETRIES: u32 = 5;
/// TTL 上限：10 年。
const MAX_ADDRESS_TTL_SECS: u64 = 3650 * 24 * 3600;

fn validate_address_label(label: &str) -> Result<String, String> {
    if label.len() > MAX_ADDRESS_LABEL_BYTES {
        return Err(format!("label 过长（上限 {MAX_ADDRESS_LABEL_BYTES} 字节）"));
    }
    if label.chars().any(char::is_control) {
        return Err("label 不能包含控制字符".into());
    }
    Ok(label.to_owned())
}

/// 仅用于形状校验的占位 id（满足 acct_ + 26 位 base32 约束）。
const VALIDATION_ONLY_ID: &str = const_format_id();

/// 同一插件 transport 不得让两个未归档账户绑定同一远端邮箱。固定文案由
/// `account_error` 映射为稳定的 ACCOUNT_CONFLICT（-32011）。
const PLUGIN_MAILBOX_BINDING_CONFLICT: &str = "插件 provider 与邮箱地址组合已存在";

/// 返回 profile 两个方向实际使用的插件 provider；同一 provider 同时负责收发时
/// 只返回一次，避免把一个 profile 自己的双向委托算成两条绑定。
fn unique_plugin_providers<'a>(
    incoming: &'a IncomingSettings,
    outgoing: Option<&'a OutgoingSettings>,
) -> Vec<&'a str> {
    let mut providers = Vec::with_capacity(2);
    if let Some(provider) = incoming.provider() {
        providers.push(provider);
    }
    if let Some(provider) = outgoing.and_then(OutgoingSettings::provider) {
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    providers
}

/// 邮箱格式先复用宿主的权威校验，再按邮件地址语义只规范化大小写不敏感的域名；
/// local part 原样保留，避免把理论上区分大小写的两个邮箱合并。
fn canonical_plugin_mailbox(address: &str) -> Result<String, ManagerError> {
    account_catalog::validate_address(address).map_err(ManagerError::Invalid)?;
    let (local, domain) = address
        .rsplit_once('@')
        .ok_or_else(|| ManagerError::Invalid("address 不是有效邮箱地址".into()))?;
    Ok(format!("{local}@{}", domain.to_ascii_lowercase()))
}

/// 必须在 `mutation_lock` 持有期间调用，令“检查 → 发布”成为同一串行事务。
fn ensure_plugin_mailbox_binding_available(
    catalog: &AccountCatalog,
    address: &str,
    incoming: &IncomingSettings,
    outgoing: Option<&OutgoingSettings>,
    ignore_account: Option<&str>,
) -> Result<(), ManagerError> {
    let providers = unique_plugin_providers(incoming, outgoing);
    if providers.is_empty() {
        return Ok(());
    }
    let mailbox = canonical_plugin_mailbox(address)?;

    for profile in &catalog.profiles {
        if profile.archived_at.is_some() || ignore_account == Some(profile.id.as_str()) {
            continue;
        }
        if canonical_plugin_mailbox(&profile.address)? != mailbox {
            continue;
        }
        let existing = unique_plugin_providers(&profile.incoming, profile.outgoing.as_ref());
        if providers.iter().any(|provider| existing.contains(provider)) {
            return Err(ManagerError::Conflict(
                PLUGIN_MAILBOX_BINDING_CONFLICT.into(),
            ));
        }
    }
    Ok(())
}

const fn const_format_id() -> &'static str {
    // 编译期拼接，避免手数 26 个字符。
    concat!("acct_", "aaaaaaaaaaaaaaaaaaaaaaaaaa")
}

#[cfg(test)]
mod id_const_tests {
    #[test]
    fn validation_only_id_has_exactly_26_base32_chars() {
        assert_eq!(
            super::account_catalog::validate_account_id(super::VALIDATION_ONLY_ID),
            Ok(())
        );
    }
}

fn truncate_short(value: &str) -> String {
    value.chars().take(40).collect()
}

fn sanitize_detail(detail: &str) -> String {
    const MAX_DETAIL_BYTES: usize = 160;
    let cleaned: String = detail.chars().filter(|c| !c.is_control()).collect();
    if cleaned.len() <= MAX_DETAIL_BYTES {
        return cleaned;
    }
    let mut end = MAX_DETAIL_BYTES;
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].to_string()
}

fn validate_secret(secret: &[u8]) -> Result<(), ManagerError> {
    if secret.is_empty() {
        return Err(ManagerError::Invalid("秘密不能为空".into()));
    }
    if secret.len() > account_catalog::MAX_SECRET_BYTES {
        return Err(ManagerError::Invalid(format!(
            "秘密超过 {} 字节上限",
            account_catalog::MAX_SECRET_BYTES
        )));
    }
    Ok(())
}

fn ensure_probe_ok(report: &ProbeReport) -> Result<(), ManagerError> {
    if !report.incoming.ok {
        return Err(ManagerError::ProbeFailed(report.clone()));
    }
    if let Some(outgoing) = &report.outgoing {
        if !outgoing.ok {
            return Err(ManagerError::ProbeFailed(report.clone()));
        }
    }
    Ok(())
}

// —— 视图 DTO ——

#[derive(Clone, Debug, Serialize)]
pub struct ProfileView {
    pub id: String,
    pub revision: u64,
    pub display_name: String,
    pub address: String,
    pub enabled: bool,
    pub archived_at: Option<u64>,
    pub source: &'static str,
    pub editable: bool,
    pub incoming: IncomingView,
    pub outgoing: Option<OutgoingView>,
    pub credentials: CredentialsView,
    pub runtime: RuntimeView,
}

#[derive(Clone, Debug, Serialize)]
pub struct IncomingView {
    pub kind: &'static str,
    pub server: String,
    pub port: u16,
    pub security: String,
    pub username: String,
    pub mailbox: String,
    pub idle: bool,
    pub poll_secs: u64,
    /// kind=plugin 时的受托插件 id（ADR-0021）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OutgoingView {
    pub kind: &'static str,
    pub server: String,
    pub port: u16,
    pub security: String,
    pub username: String,
    pub credential: String,
    /// kind=plugin 时的受托插件 id（ADR-0021）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CredentialsView {
    /// saved | missing | unknown | shared | embedded
    pub incoming: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeView {
    /// connecting | online | reconnecting | disabled | needs_credentials | error
    pub state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// `detail` 的稳定 code（ADR-0048 L6），与 `account/state` 同一套。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_code: Option<crate::events::AccountDetailCode>,
    /// 服务器或系统原文（不含我们的前缀），可空。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub server_detail: String,
}

impl RuntimeView {
    /// 由收信循环的状态投影：`detail` 与 `server_detail` 有界脱敏，code 原样。
    fn from_info(state: &str, info: Option<&crate::events::AccountStateInfo>) -> Self {
        RuntimeView {
            state: state.to_string(),
            detail: info.map(|info| sanitize_detail(&info.detail)).unwrap_or_default(),
            detail_code: info.and_then(|info| info.detail_code),
            server_detail: info
                .map(|info| sanitize_detail(&info.server_detail))
                .unwrap_or_default(),
        }
    }
}

enum RuntimeState {
    Connecting,
    Online,
    Reconnecting,
    Disabled,
    NeedsCredentials,
    Error,
}

impl RuntimeState {
    fn label(self) -> &'static str {
        match self {
            RuntimeState::Connecting => "connecting",
            RuntimeState::Online => "online",
            RuntimeState::Reconnecting => "reconnecting",
            RuntimeState::Disabled => "disabled",
            RuntimeState::NeedsCredentials => "needs_credentials",
            RuntimeState::Error => "error",
        }
    }
}

fn legacy_view(
    name: &str,
    cfg: &ReceiveConfig,
    state: crate::events::AccountStateInfo,
) -> ProfileView {
    ProfileView {
        id: name.to_string(),
        revision: 0,
        display_name: name.to_string(),
        address: cfg.username.clone(),
        enabled: true,
        archived_at: None,
        source: "legacy",
        editable: false,
        incoming: IncomingView {
            kind: "imap",
            server: cfg.server.clone(),
            port: cfg.port,
            security: cfg.tls.clone(),
            username: cfg.username.clone(),
            mailbox: cfg.mailbox.clone(),
            idle: cfg.idle,
            poll_secs: cfg.poll_secs,
            provider: None,
        },
        outgoing: None,
        credentials: CredentialsView {
            incoming: "embedded".into(),
            outgoing: None,
        },
        runtime: RuntimeView::from_info(
            match state.state {
                crate::events::AccountStateKind::Connecting => "connecting",
                crate::events::AccountStateKind::Online => "online",
                crate::events::AccountStateKind::Reconnecting => "reconnecting",
                crate::events::AccountStateKind::Offline => "error",
            },
            Some(&state),
        ),
    }
}
