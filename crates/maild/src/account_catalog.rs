//! account_catalog：受管账户目录 `accounts.toml`（ADR-0015 §1/§2）。
//!
//! 边界：
//! - 仅非秘密配置；秘密只进平台凭据库（`crate::credential`）；
//! - 仅由 Agent 写入；同目录临时文件 + fsync + 原子 rename + 父目录 fsync 发布；
//! - owner 必须是当前用户，普通文件、单硬链接、无符号链接，Unix mode 0600；
//! - 未知 `format_version` 在任何凭据或网络副作用之前 fail-closed；
//! - 首次保存才创建文件，空安装不制造额外状态。

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 当前目录格式版本。未来版本在任何副作用前拒绝。
/// v2（M4-05，ADR-0021）：新增委托变体 `kind="plugin"`；读取兼容 v1
/// （v1 文件禁止 plugin 变体），任何写入一律落盘 v2。
pub const CATALOG_FORMAT_VERSION: u64 = 2;
/// 仍可读取的最老目录版本（v1：仅内置 imap/smtp 变体）。
pub const MIN_SUPPORTED_FORMAT_VERSION: u64 = 1;
/// 未归档 profile 数量上限。
pub const MAX_UNARCHIVED_PROFILES: usize = 64;
pub const MAX_DISPLAY_NAME_BYTES: usize = 128;
pub const MAX_ADDRESS_BYTES: usize = 320;
pub const MAX_USERNAME_BYTES: usize = 320;
pub const MAX_HOST_BYTES: usize = 253;
pub const MAX_MAILBOX_BYTES: usize = 255;
/// 单条秘密字节上限。
pub const MAX_SECRET_BYTES: usize = 4096;
/// 凭据库 service 名（不随 bundle ID 改名，见 ADR-0015 §5；取自 `brand.toml` 的
/// `keychain_service`）。
pub const CREDENTIAL_SERVICE: &str = maild_brand::KEYCHAIN_SERVICE;
/// 目录文件读取上限（防误配超大文件拖垮启动）。
const MAX_CATALOG_BYTES: u64 = 1024 * 1024;

// —— 数据模型 ——

/// 受管账户允许的传输安全模式。明文与自签开关只保留给旧配置和 hermetic 测试。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecurityMode {
    /// TLS 直连（993/465 风格）。
    Implicit,
    /// 明文连接后协商 STARTTLS。
    StartTls,
}

impl SecurityMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SecurityMode::Implicit => "implicit",
            SecurityMode::StartTls => "starttls",
        }
    }
}

/// 收信 endpoint。运行时始终保存已解析的显式配置，preset 不进入本 schema。
/// `Plugin`（M4-05，ADR-0021）：收信整体委托给受授权插件，凭据不经过本目录。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum IncomingSettings {
    Imap {
        server: String,
        port: u16,
        security: SecurityMode,
        username: String,
        mailbox: String,
        #[serde(default = "default_idle")]
        idle: bool,
        #[serde(default = "default_poll_secs")]
        poll_secs: u64,
    },
    Plugin {
        provider: String,
        /// 委托收信的轮询间隔（秒）。v1 仅 poll（ADR-0021 §4：间隔复用 poll_secs）。
        #[serde(default = "default_poll_secs")]
        poll_secs: u64,
    },
}

fn default_idle() -> bool {
    true
}

fn default_poll_secs() -> u64 {
    60
}

impl IncomingSettings {
    /// 解构 IMAP 字段；委托变体返回 None。
    pub fn imap(
        &self,
    ) -> Option<(
        &str,
        u16,
        SecurityMode,
        &str,
        &str,
        bool,
        u64,
    )> {
        match self {
            IncomingSettings::Imap {
                server,
                port,
                security,
                username,
                mailbox,
                idle,
                poll_secs,
            } => Some((server, *port, *security, username, mailbox, *idle, *poll_secs)),
            IncomingSettings::Plugin { .. } => None,
        }
    }

    /// 委托方向的受托插件 id。
    pub fn provider(&self) -> Option<&str> {
        match self {
            IncomingSettings::Imap { .. } => None,
            IncomingSettings::Plugin { provider, .. } => Some(provider),
        }
    }

    /// 委托方向的轮询间隔（内置 IMAP 复用其字段；此处统一出口）。
    pub fn poll_secs(&self) -> u64 {
        match self {
            IncomingSettings::Imap { poll_secs, .. } => *poll_secs,
            IncomingSettings::Plugin { poll_secs, .. } => *poll_secs,
        }
    }
}

/// SMTP 密码复用方式：与收件相同，或独立保存一份。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialSharing {
    Incoming,
    Separate,
}

/// 发信 endpoint（可省略）。`Plugin`（M4-05，ADR-0021）：发信整体委托给
/// 受授权插件；`from` 由内核填 profile.address，账户凭据不跨缝。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum OutgoingSettings {
    Smtp {
        server: String,
        port: u16,
        security: SecurityMode,
        username: String,
        credential: CredentialSharing,
    },
    Plugin {
        provider: String,
    },
}

impl OutgoingSettings {
    pub fn smtp(&self) -> Option<(&str, u16, SecurityMode, &str, CredentialSharing)> {
        match self {
            OutgoingSettings::Smtp {
                server,
                port,
                security,
                username,
                credential,
            } => Some((server, *port, *security, username, *credential)),
            OutgoingSettings::Plugin { .. } => None,
        }
    }

    /// 委托方向的受托插件 id。
    pub fn provider(&self) -> Option<&str> {
        match self {
            OutgoingSettings::Smtp { .. } => None,
            OutgoingSettings::Plugin { provider } => Some(provider),
        }
    }
}

/// 单个受管账户的非秘密快照。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountProfile {
    /// `acct_` + 26 位小写 base32 随机标识；创建后不可修改。
    pub id: String,
    /// 从 1 开始，每次成功修改递增（乐观并发控制）。
    pub revision: u64,
    pub display_name: String,
    /// 默认发件地址与账户展示地址。
    pub address: String,
    pub enabled: bool,
    /// 非空表示已移除（软归档）；不挂载且不得保留有效凭据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<u64>,
    /// 当前 IMAP 凭据 item revision。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incoming_credential_revision: Option<u32>,
    /// 独立 SMTP 凭据 item revision；复用收件密码时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing_credential_revision: Option<u32>,
    pub incoming: IncomingSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<OutgoingSettings>,
}

/// `accounts.toml` 的内存形态。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountCatalog {
    pub format_version: u64,
    #[serde(default)]
    pub profiles: Vec<AccountProfile>,
}

impl AccountCatalog {
    pub fn empty() -> Self {
        Self {
            format_version: CATALOG_FORMAT_VERSION,
            profiles: Vec::new(),
        }
    }

    pub fn profile(&self, id: &str) -> Option<&AccountProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn profile_mut(&mut self, id: &str) -> Option<&mut AccountProfile> {
        self.profiles.iter_mut().find(|p| p.id == id)
    }

    /// 全量校验（保存前与加载后都必须通过）。
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.format_version != CATALOG_FORMAT_VERSION {
            return Err(CatalogError::FutureVersion(self.format_version));
        }
        let mut ids = std::collections::HashSet::new();
        let mut unarchived = 0usize;
        for profile in &self.profiles {
            validate_profile(profile).map_err(CatalogError::Invalid)?;
            if !ids.insert(profile.id.as_str()) {
                return Err(CatalogError::Invalid(format!(
                    "重复的账户 id: {}",
                    profile.id
                )));
            }
            if profile.archived_at.is_none() {
                unarchived += 1;
            }
        }
        if unarchived > MAX_UNARCHIVED_PROFILES {
            return Err(CatalogError::Invalid(format!(
                "未归档账户超过上限 {MAX_UNARCHIVED_PROFILES}"
            )));
        }
        Ok(())
    }
}

// —— 校验 ——

/// 校验单个 profile 的字段边界（ADR-0015 §2 的首版上限）。
pub fn validate_profile(profile: &AccountProfile) -> Result<(), String> {
    validate_account_id(&profile.id)?;
    if profile.revision < 1 {
        return Err(format!("账户 {} revision 必须从 1 开始", profile.id));
    }
    validate_display_name(&profile.display_name)?;
    validate_address(&profile.address)?;
    if profile.enabled && profile.archived_at.is_some() {
        return Err(format!("账户 {} 已归档则不得处于启用状态", profile.id));
    }
    if profile.archived_at.is_some()
        && (profile.incoming_credential_revision.is_some()
            || profile.outgoing_credential_revision.is_some())
    {
        return Err(format!("账户 {} 已归档则不得引用有效凭据", profile.id));
    }
    // 委托方向（ADR-0021）没有本目录凭据；凭据 revision 必须为空。
    if matches!(profile.incoming, IncomingSettings::Plugin { .. })
        && profile.incoming_credential_revision.is_some()
    {
        return Err(format!(
            "账户 {} incoming 委托给插件时不得保留收信凭据 revision",
            profile.id
        ));
    }
    match &profile.incoming {
        IncomingSettings::Imap { .. } => {
            let (server, port, _security, username, mailbox, _idle, _poll_secs) =
                profile.incoming.imap().expect("Imap 变体已匹配");
            validate_host("incoming.server", server)?;
            validate_port(port)?;
            validate_username(username)?;
            validate_mailbox_field(mailbox)?;
        }
        IncomingSettings::Plugin { provider, .. } => {
            validate_provider_id(provider)
                .map_err(|reason| format!("账户 {} incoming.{reason}", profile.id))?;
        }
    }
    if profile.incoming.poll_secs() < 1 {
        return Err(format!("账户 {} incoming.poll_secs 必须 ≥ 1", profile.id));
    }
    match &profile.outgoing {
        Some(OutgoingSettings::Smtp { .. }) => {
            let (server, port, _security, username, sharing) =
                profile.outgoing.as_ref().and_then(|o| o.smtp()).expect("Smtp 变体已匹配");
            validate_host("outgoing.server", server)?;
            validate_port(port)?;
            validate_username(username)?;
            // 复用收件密码要求收信侧是内置 IMAP（委托方向没有本目录凭据）。
            if matches!(sharing, CredentialSharing::Incoming)
                && matches!(profile.incoming, IncomingSettings::Plugin { .. })
            {
                return Err(format!(
                    "账户 {} outgoing 复用收件密码要求 incoming 为内置 IMAP",
                    profile.id
                ));
            }
        }
        Some(OutgoingSettings::Plugin { provider }) => {
            validate_provider_id(provider)
                .map_err(|reason| format!("账户 {} outgoing.{reason}", profile.id))?;
            if profile.outgoing_credential_revision.is_some() {
                return Err(format!(
                    "账户 {} outgoing 委托给插件时不得保留发信凭据 revision",
                    profile.id
                ));
            }
        }
        None => {}
    }
    Ok(())
}

/// 受托插件 id 语法（与 external 包 id 同一语法；只校验形状，不要求已安装——
/// 委托是惰性绑定，未就绪时调用期报 -32018）。
pub fn validate_provider_id(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    if value.is_empty()
        || value.len() > 128
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("provider 插件 id 无效".into());
    }
    Ok(())
}

/// `acct_` + 26 位小写 base32（RFC 4648 字母表的小写形式）。
pub fn validate_account_id(id: &str) -> Result<(), String> {
    let rest = id
        .strip_prefix("acct_")
        .ok_or_else(|| format!("账户 id 必须以 acct_ 开头: {id}"))?;
    if rest.len() != 26 || !rest.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
        return Err(format!("账户 id 必须是 acct_ 加 26 位小写 base32: {id}"));
    }
    if rest.bytes().any(|b| !BASE32_LOWER.contains(&b)) {
        return Err(format!("账户 id 含 base32 字母表之外的字符: {id}"));
    }
    Ok(())
}

pub fn validate_display_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("display_name 不能为空".into());
    }
    if name.len() > MAX_DISPLAY_NAME_BYTES {
        return Err(format!(
            "display_name 过长（上限 {MAX_DISPLAY_NAME_BYTES} 字节）"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("display_name 不能包含控制字符".into());
    }
    Ok(())
}

pub fn validate_address(address: &str) -> Result<(), String> {
    if address.len() > MAX_ADDRESS_BYTES {
        return Err(format!("address 过长（上限 {MAX_ADDRESS_BYTES} 字节）"));
    }
    crate::transport::validate_mailbox("address", address)
}

fn validate_host(label: &str, host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err(format!("{label} 不能为空"));
    }
    if host.len() > MAX_HOST_BYTES {
        return Err(format!("{label} 过长（上限 {MAX_HOST_BYTES} 字节）"));
    }
    if host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!("{label} 不能包含空白或控制字符"));
    }
    Ok(())
}

fn validate_port(port: u16) -> Result<(), String> {
    if port == 0 {
        return Err("port 必须在 1..=65535".into());
    }
    Ok(())
}

fn validate_username(username: &str) -> Result<(), String> {
    if username.is_empty() {
        return Err("username 不能为空".into());
    }
    if username.len() > MAX_USERNAME_BYTES {
        return Err(format!("username 过长（上限 {MAX_USERNAME_BYTES} 字节）"));
    }
    if username.chars().any(char::is_control) {
        return Err("username 不能包含控制字符".into());
    }
    Ok(())
}

fn validate_mailbox_field(mailbox: &str) -> Result<(), String> {
    if mailbox.is_empty() {
        return Err("mailbox 不能为空".into());
    }
    if mailbox.len() > MAX_MAILBOX_BYTES {
        return Err(format!("mailbox 过长（上限 {MAX_MAILBOX_BYTES} 字节）"));
    }
    if mailbox.chars().any(char::is_control) {
        return Err("mailbox 不能包含控制字符".into());
    }
    Ok(())
}

/// 生成新的账户 id（130 位随机熵）。
pub fn generate_account_id() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut random = [0u8; 26];
    getrandom::fill(&mut random).expect("系统随机源不可用");
    let mut id = String::with_capacity(31);
    id.push_str("acct_");
    for byte in random {
        id.push(ALPHABET[(byte & 31) as usize] as char);
    }
    debug_assert!(validate_account_id(&id).is_ok());
    id
}

// —— 凭据键 ——

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialRole {
    Incoming,
    Outgoing,
}

impl CredentialRole {
    pub fn as_str(self) -> &'static str {
        match self {
            CredentialRole::Incoming => "incoming",
            CredentialRole::Outgoing => "outgoing",
        }
    }
}

/// 平台凭据库中的 item 定位：service 固定，account 为 `<id>/<role>/<revision>`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialKey {
    pub account_id: String,
    pub role: CredentialRole,
    pub revision: u32,
}

impl CredentialKey {
    pub fn new(account_id: impl Into<String>, role: CredentialRole, revision: u32) -> Self {
        Self {
            account_id: account_id.into(),
            role,
            revision,
        }
    }

    /// Keychain generic password item 的 account 属性。
    pub fn attr_account(&self) -> String {
        format!(
            "{}/{}/{}",
            self.account_id,
            self.role.as_str(),
            self.revision
        )
    }

    /// 从枚举出的 item account 属性还原键；不属于本产品命名约定时返回 None。
    pub fn parse(attr: &str) -> Option<Self> {
        let (account_id, rest) = attr.split_once('/')?;
        let (role, revision) = rest.rsplit_once('/')?;
        let role = match role {
            "incoming" => CredentialRole::Incoming,
            "outgoing" => CredentialRole::Outgoing,
            _ => return None,
        };
        let revision = revision.parse().ok()?;
        if validate_account_id(account_id).is_err() {
            return None;
        }
        Some(Self {
            account_id: account_id.to_string(),
            role,
            revision,
        })
    }
}

// —— 文件 IO ——

/// 加载受管账户目录。文件不存在返回 `Ok(None)`（空安装不制造状态）。
pub fn load_catalog(path: &Path) -> Result<Option<AccountCatalog>, CatalogError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CatalogError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    check_file_shape(path, &metadata)?;
    let bytes = std::fs::read(path).map_err(|source| CatalogError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if bytes.len() as u64 > MAX_CATALOG_BYTES {
        return Err(CatalogError::Invalid(format!(
            "accounts.toml 超过 {MAX_CATALOG_BYTES} 字节上限"
        )));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        CatalogError::Invalid("accounts.toml 不是有效的 UTF-8".into())
    })?;
    let mut catalog: AccountCatalog = toml::from_str(&text)
        .map_err(|error| CatalogError::Parse(error.to_string()))?;
    // 双读（ADR-0021）：接受 v1 与 v2；v1 文件禁止 plugin 变体（防御手编，
    // 正常 v1 产物不可能出现）。其余版本在任何副作用前 fail-closed。
    if !matches!(
        catalog.format_version,
        MIN_SUPPORTED_FORMAT_VERSION..=CATALOG_FORMAT_VERSION
    ) {
        return Err(CatalogError::FutureVersion(catalog.format_version));
    }
    if catalog.format_version < CATALOG_FORMAT_VERSION {
        for profile in &catalog.profiles {
            if matches!(profile.incoming, IncomingSettings::Plugin { .. })
                || matches!(profile.outgoing, Some(OutgoingSettings::Plugin { .. }))
            {
                return Err(CatalogError::Invalid(format!(
                    "账户 {} 在 format_version 1 下不得使用 plugin 委托变体",
                    profile.id
                )));
            }
        }
    }
    // validate() 只接受当前内存版本；兼容格式完成专属门禁后先归一化，
    // 后续全量校验与任何写入都按 v2 契约执行。
    catalog.format_version = CATALOG_FORMAT_VERSION;
    catalog.validate()?;
    Ok(Some(catalog))
}

/// 原子发布账户目录：校验 → 同目录临时文件（0600）→ fsync → rename → 父目录 fsync。
pub fn save_catalog(path: &Path, catalog: &AccountCatalog) -> Result<(), CatalogError> {
    catalog.validate()?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| CatalogError::Invalid(format!("账户目录路径缺少父目录: {path:?}")))?;
    check_private_dir(parent)?;

    let body = toml::to_string_pretty(catalog)
        .map_err(|error| CatalogError::Invalid(format!("账户目录序列化失败: {error}")))?;

    // 复用同一随机源生成唯一临时名；发布失败时清理残留。
    let temp = parent.join(format!(".accounts.toml.tmp-{}", generate_account_id()));
    if let Err(error) = publish_atomically(&temp, path, body.as_bytes()) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

fn publish_atomically(temp: &Path, target: &Path, body: &[u8]) -> Result<(), CatalogError> {
    use std::io::Write as _;

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(temp)
            .map_err(|source| CatalogError::Io {
                path: temp.to_path_buf(),
                source,
            })?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(|source| CatalogError::Io {
            path: temp.to_path_buf(),
            source,
        })?;

    file.write_all(body).map_err(|source| CatalogError::Io {
        path: temp.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| CatalogError::Io {
        path: temp.to_path_buf(),
        source,
    })?;
    drop(file);

    std::fs::rename(temp, target).map_err(|source| CatalogError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    sync_dir(
        target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )
}

fn sync_dir(dir: &Path) -> Result<(), CatalogError> {
    #[cfg(unix)]
    {
        let file = std::fs::File::open(dir).map_err(|source| CatalogError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        file.sync_all().map_err(|source| CatalogError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}


/// 文件形状检查：普通文件、非符号链接、单硬链接、当前用户、0600。
fn check_file_shape(path: &Path, metadata: &std::fs::Metadata) -> Result<(), CatalogError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CatalogError::UnsafeShape {
                path: path.to_path_buf(),
                reason: "必须是普通文件且不能是符号链接".into(),
            });
        }
        if metadata.nlink() != 1 {
            return Err(CatalogError::UnsafeShape {
                path: path.to_path_buf(),
                reason: format!("必须只有单个硬链接（当前 {}）", metadata.nlink()),
            });
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(CatalogError::UnsafeShape {
                path: path.to_path_buf(),
                reason: "owner 必须是当前用户".into(),
            });
        }
        let mode = metadata.permissions().mode();
        if mode & 0o177 != 0 {
            return Err(CatalogError::UnsafeShape {
                path: path.to_path_buf(),
                reason: format!("权限必须是 0600（当前 {:o}）", mode & 0o777),
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, metadata);
    }
    Ok(())
}

fn check_private_dir(dir: &Path) -> Result<(), CatalogError> {
    let metadata =
        std::fs::symlink_metadata(dir).map_err(|source| CatalogError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CatalogError::UnsafeShape {
            path: dir.to_path_buf(),
            reason: "父目录必须是非符号链接目录".into(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(CatalogError::UnsafeShape {
                path: dir.to_path_buf(),
                reason: "父目录必须由当前用户独占（0700）".into(),
            });
        }
    }
    Ok(())
}

// —— 错误 ——

#[derive(Debug)]
pub enum CatalogError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse(String),
    /// 未知未来版本：必须在任何凭据或网络副作用前拒绝。
    FutureVersion(u64),
    Invalid(String),
    UnsafeShape {
        path: PathBuf,
        reason: String,
    },
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::Io { path, source } => {
                write!(f, "访问账户目录 {path:?} 失败: {source}")
            }
            CatalogError::Parse(detail) => write!(f, "账户目录解析失败: {detail}"),
            CatalogError::FutureVersion(version) => write!(
                f,
                "账户目录 format_version {version} 高于本程序支持的 {CATALOG_FORMAT_VERSION}，拒绝处理"
            ),
            CatalogError::Invalid(detail) => write!(f, "账户目录内容无效: {detail}"),
            CatalogError::UnsafeShape { path, reason } => {
                write!(f, "账户目录 {path:?} 形状不安全: {reason}")
            }
        }
    }
}

impl std::error::Error for CatalogError {}

const BASE32_LOWER: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Write as _;

    #[test]
    fn valid_v1_catalog_is_upgraded_in_memory_before_validation() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                dir.path(),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let path = dir.path().join("accounts.toml");
        let profile = AccountProfile {
            id: "acct_abcdefghijklmnopqrstuvwxyz0".chars().take(31).collect(),
            revision: 1,
            display_name: "Legacy account".into(),
            address: "me@example.test".into(),
            enabled: true,
            archived_at: None,
            incoming_credential_revision: Some(1),
            outgoing_credential_revision: None,
            incoming: IncomingSettings::Imap {
                server: "imap.example.test".into(),
                port: 993,
                security: SecurityMode::Implicit,
                username: "me@example.test".into(),
                mailbox: "INBOX".into(),
                idle: true,
                poll_secs: 60,
            },
            outgoing: None,
        };
        let mut catalog = AccountCatalog::empty();
        catalog.profiles.push(profile);
        save_catalog(&path, &catalog).unwrap();

        let v2 = std::fs::read_to_string(&path).unwrap();
        let v1 = v2.replacen("format_version = 2", "format_version = 1", 1);
        assert_ne!(v1, v2);
        std::fs::write(&path, v1).unwrap();

        let loaded = load_catalog(&path).unwrap().unwrap();
        assert_eq!(loaded.format_version, CATALOG_FORMAT_VERSION);
        assert_eq!(loaded.profiles.len(), 1);
        assert!(matches!(
            loaded.profiles[0].incoming,
            IncomingSettings::Imap { .. }
        ));
    }

    /// M4-05（ADR-0021）：委托 profile 的 TOML 往返 + v1 目录禁 plugin 变体。
    #[test]
    fn plugin_variants_round_trip_and_v1_rejects_them() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                dir.path(),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        #[cfg(not(unix))]
        let _ = &dir;
        let path = dir.path().join("accounts.toml");
        let profile = AccountProfile {
            id: "acct_abcdefghijklmnopqrstuvwxyz0".chars().take(31).collect(),
            revision: 1,
            display_name: "委托邮箱".into(),
            address: "me@example.test".into(),
            enabled: true,
            archived_at: None,
            incoming_credential_revision: None,
            outgoing_credential_revision: None,
            incoming: IncomingSettings::Plugin {
                provider: "org.example.transport".into(),
                poll_secs: 30,
            },
            outgoing: Some(OutgoingSettings::Plugin {
                provider: "org.example.transport".into(),
            }),
        };
        // id 必须满足 acct_ + 26 位小写 base32；上面 take(31) 恰好构造合法形状。
        assert!(validate_account_id(&profile.id).is_ok(), "{}", profile.id);
        let mut catalog = AccountCatalog::empty();
        catalog.profiles.push(profile);
        catalog.validate().unwrap();
        save_catalog(&path, &catalog).unwrap();

        let reloaded = load_catalog(&path).unwrap().unwrap();
        assert_eq!(reloaded.format_version, CATALOG_FORMAT_VERSION);
        let loaded_profile = &reloaded.profiles[0];
        assert_eq!(
            loaded_profile.incoming.provider(),
            Some("org.example.transport")
        );
        assert_eq!(
            loaded_profile.outgoing.as_ref().unwrap().provider(),
            Some("org.example.transport")
        );

        // v1 文件带 plugin 变体（手编防御）：拒绝且不清理。
        std::fs::write(
            &path,
            r#"
format_version = 1
[[profiles]]
id = "acct_abcdefghijklmnopqrstuvwxyz0"
revision = 1
display_name = "x"
address = "me@example.test"
enabled = true

[profiles.incoming]
kind = "plugin"
provider = "org.example.transport"
"#,
        )
        .unwrap();
        let error = load_catalog(&path).unwrap_err();
        assert!(matches!(error, CatalogError::Invalid(_)), "{error:?}");

        // 未来版本照旧 fail-closed。
        std::fs::write(&path, "format_version = 999\n").unwrap();
        let error = load_catalog(&path).unwrap_err();
        assert!(matches!(error, CatalogError::FutureVersion(999)));
    }


    fn imap(server: &str) -> IncomingSettings {
        IncomingSettings::Imap {
            server: server.into(),
            port: 993,
            security: SecurityMode::Implicit,
            username: "user@example.test".into(),
            mailbox: "INBOX".into(),
            idle: true,
            poll_secs: 60,
        }
    }

    fn smtp() -> OutgoingSettings {
        OutgoingSettings::Smtp {
            server: "smtp.example.test".into(),
            port: 587,
            security: SecurityMode::StartTls,
            username: "user@example.test".into(),
            credential: CredentialSharing::Incoming,
        }
    }

    fn sample(id: &str) -> AccountProfile {
        AccountProfile {
            id: id.into(),
            revision: 1,
            display_name: "Main".into(),
            address: "user@example.test".into(),
            enabled: true,
            archived_at: None,
            incoming_credential_revision: Some(1),
            outgoing_credential_revision: None,
            incoming: imap("imap.example.test"),
            outgoing: Some(smtp()),
        }
    }

    #[test]
    fn generated_ids_match_the_frozen_shape() {
        for _ in 0..32 {
            let id = generate_account_id();
            assert_eq!(id.len(), 31);
            assert!(validate_account_id(&id).is_ok());
        }
        assert!(validate_account_id("acct_").is_err());
        assert!(validate_account_id("main").is_err());
        assert!(validate_account_id("acct_ABCDEFGHIJKLMNOPQRSTUVWXYZ234567").is_err());
    }

    #[test]
    fn profile_validation_enforces_field_caps_and_archive_rules() {
        let mut profile = sample(&generate_account_id());
        assert!(validate_profile(&profile).is_ok());

        profile.display_name = "x".repeat(MAX_DISPLAY_NAME_BYTES + 1);
        assert!(validate_profile(&profile).is_err());
        profile.display_name = "bad\nname".into();
        assert!(validate_profile(&profile).is_err());
        profile.display_name = "Main".into();

        profile.address = "not-an-address".into();
        assert!(validate_profile(&profile).is_err());
        profile.address = "user@example.test".into();

        profile.revision = 0;
        assert!(validate_profile(&profile).is_err());
        profile.revision = 1;

        profile.archived_at = Some(123);
        assert!(validate_profile(&profile).is_err(), "归档不得引用凭据");
        profile.incoming_credential_revision = None;
        profile.outgoing_credential_revision = Some(2);
        assert!(validate_profile(&profile).is_err());
        profile.outgoing_credential_revision = None;
        profile.enabled = true;
        assert!(validate_profile(&profile).is_err(), "归档不得启用");
        profile.enabled = false;
        assert!(validate_profile(&profile).is_ok());

        let mut bad = sample(&generate_account_id());
        bad.incoming = IncomingSettings::Imap {
            server: "imap.example.test".into(),
            port: 0,
            security: SecurityMode::Implicit,
            username: "u".into(),
            mailbox: "INBOX".into(),
            idle: true,
            poll_secs: 60,
        };
        assert!(validate_profile(&bad).is_err());
    }

    #[test]
    fn catalog_roundtrips_through_toml_and_rejects_future_versions() {
        let dir = tempfile::tempdir().unwrap();
        // 发布要求父目录当前用户独占（0700）；测试里显式准备一个私有目录。
        let config_dir = dir.path().join("config");
        std::fs::create_dir(&config_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = config_dir.join("accounts.toml");
        assert!(load_catalog(&path).unwrap().is_none(), "空安装无文件");

        let mut catalog = AccountCatalog::empty();
        catalog.profiles.push(sample(&generate_account_id()));
        save_catalog(&path, &catalog).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let loaded = load_catalog(&path).unwrap().unwrap();
        assert_eq!(loaded, catalog);

        let future = r#"
            format_version = 999
            [[profiles]]
            id = "acct_abcdefghijklmnopqrstuvwxyz23"
            revision = 1
            display_name = "x"
            address = "a@b.test"
            enabled = false
            [profiles.incoming]
            kind = "imap"
            server = "imap.example.test"
            port = 993
            security = "implicit"
            username = "u@b.test"
            mailbox = "INBOX"
            idle = true
            poll_secs = 60
        "#;
        let parsed: AccountCatalog = toml::from_str(future).unwrap();
        assert!(matches!(
            parsed.validate(),
            Err(CatalogError::FutureVersion(999))
        ));
        let future_path = config_dir.join("future.toml");
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&future_path)
                .unwrap()
                .write_all(future.as_bytes())
                .unwrap();
        }
        #[cfg(not(unix))]
        std::fs::write(&future_path, future).unwrap();
        assert!(matches!(
            load_catalog(&future_path),
            Err(CatalogError::FutureVersion(999))
        ));

        std::fs::write(&path, "format_version = 1\n[[profiles]]\nbroken").unwrap();
        assert!(matches!(load_catalog(&path), Err(CatalogError::Parse(_))));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_file_shapes_fail_closed() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("config");
        std::fs::create_dir(&config_dir).unwrap();
        std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = config_dir.join("accounts.toml");

        let mut catalog = AccountCatalog::empty();
        catalog.profiles.push(sample(&generate_account_id()));
        save_catalog(&path, &catalog).unwrap();

        // 宽权限
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            load_catalog(&path),
            Err(CatalogError::UnsafeShape { .. })
        ));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        // 第二硬链接
        let link = dir.path().join("hardlink.toml");
        std::fs::hard_link(&path, &link).unwrap();
        assert!(matches!(
            load_catalog(&path),
            Err(CatalogError::UnsafeShape { .. })
        ));
        std::fs::remove_file(&link).unwrap();

        // 符号链接
        let target = dir.path().join("real.toml");
        std::fs::write(&target, "format_version = 1\n").unwrap();
        let alias = dir.path().join("alias.toml");
        symlink(&target, &alias).unwrap();
        assert!(matches!(
            load_catalog(&alias),
            Err(CatalogError::UnsafeShape { .. })
        ));

        // 宽权限父目录拒绝写入
        let loose = tempfile::tempdir().unwrap();
        let loose_dir = loose.path().join("config");
        std::fs::create_dir(&loose_dir).unwrap();
        std::fs::set_permissions(&loose_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(save_catalog(&loose_dir.join("accounts.toml"), &catalog).is_err());
    }

    #[test]
    fn duplicate_ids_and_cap_are_rejected() {
        let mut catalog = AccountCatalog::empty();
        let id = generate_account_id();
        catalog.profiles.push(sample(&id));
        catalog.profiles.push(sample(&id));
        assert!(catalog.validate().is_err());

        let mut many = AccountCatalog::empty();
        for _ in 0..=MAX_UNARCHIVED_PROFILES {
            many.profiles.push(sample(&generate_account_id()));
        }
        assert!(many.validate().is_err());
        many.profiles.pop();
        assert!(many.validate().is_ok());
    }

    #[test]
    fn credential_keys_roundtrip_through_the_attr_shape() {
        let key = CredentialKey::new(generate_account_id(), CredentialRole::Outgoing, 7);
        let attr = key.attr_account();
        assert_eq!(CredentialKey::parse(&attr).unwrap(), key);
        assert!(CredentialKey::parse("other-service-item").is_none());
        assert!(CredentialKey::parse("acct_short/incoming/1").is_none());
        assert!(CredentialKey::parse(&format!("{}/bogus/1", generate_account_id())).is_none());
    }
}
