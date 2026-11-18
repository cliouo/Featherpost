//! plugin_identity：插件主体与逐插件凭证（ADR-0017，M4-01）。
//!
//! 信任边界是契约性的：本缝让**合作**插件获得可归因、可吊销、可限权的身份；
//! 拒绝认证的进程与普通用户工具不可区分，同用户域内的对抗归 M5 沙箱。
//!
//! - 凭证格式 `mdp_` + base64url(32B)，明文只在发放（reveal / spawn 注入）瞬间存在；
//! - 注册表 `plugin_auth.toml` 与活动配置同目录，盘上只存 SHA-256 哈希；
//!   文件纪律对齐 accounts.toml：仅 Agent 写入、0600、单硬链接、拒符号链接、
//!   64 KiB 上限、未知 `format_version` fail-closed、原子发布；
//! - 任何加载/发布失败都让身份缝整体停摆（`Unavailable`）：supervised 拒绝 spawn、
//!   `authenticate` 返回 -32015，核心其余能力照常。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cordis::Service;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// 认证凭证前缀；完整格式 `mdp_` + 43 个 base64url 字符（32 随机字节，无填充）。
pub const TOKEN_PREFIX: &str = "mdp_";
/// supervised 子进程接收凭证的环境变量（spawn 空环境中除配置环境变量外的唯一项）。
pub const TOKEN_ENV: &str = maild_brand::PLUGIN_TOKEN_ENV;

const TOKEN_RANDOM_BYTES: usize = 32;
const TOKEN_BODY_LEN: usize = 43;
const REGISTRY_FILE: &str = "plugin_auth.toml";
const REGISTRY_FORMAT_VERSION: u64 = 1;
const MAX_REGISTRY_BYTES: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 128;
const TOKEN_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// 会话主体。绑定发生在连接 accept 后、首个请求前，之后连接内不可变。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Principal {
    /// 缺省：未认证的连接，与历史行为完全一致。
    User,
    /// external package（`plugins/<id>/`）。
    Plugin(String),
    /// frontend package（`frontends/<id>/`）。
    Frontend(String),
}

impl Principal {
    /// 诊断与 `whoami` 使用的稳定字符串形态。
    pub fn as_str(&self) -> String {
        match self {
            Self::User => "user".into(),
            Self::Plugin(id) => format!("plugin:{id}"),
            Self::Frontend(id) => format!("frontend:{id}"),
        }
    }
}

/// 受管 package 的两类命名空间。id 都必须等于受管目录名。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginKind {
    External,
    Frontend,
}

impl PluginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::External => "plugin",
            Self::Frontend => "frontend",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "plugin" => Some(Self::External),
            "frontend" => Some(Self::Frontend),
            _ => None,
        }
    }
}

// —— 凭证 ——

/// 生成一条新凭证明文。只在发放瞬间持有，绝不落盘、绝不进日志。
pub fn generate_token() -> String {
    let mut random = [0u8; TOKEN_RANDOM_BYTES];
    getrandom::fill(&mut random).expect("系统随机源不可用");
    format!("{TOKEN_PREFIX}{}", base64url_encode(&random))
}

fn base64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (chunk[0] as u32) << 16
            | (chunk.get(1).copied().unwrap_or(0) as u32) << 8
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(TOKEN_ALPHABET[(word >> 18) as usize & 63] as char);
        out.push(TOKEN_ALPHABET[(word >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TOKEN_ALPHABET[(word >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(TOKEN_ALPHABET[word as usize & 63] as char);
        }
    }
    out
}

/// 注册表只保存这个哈希；验证走常量时间比较。
pub fn hash_token(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn token_well_formed(token: &str) -> bool {
    let body = token.strip_prefix(TOKEN_PREFIX);
    let Some(body) = body else {
        return false;
    };
    body.len() == TOKEN_BODY_LEN
        && body
            .bytes()
            .all(|byte| TOKEN_ALPHABET.contains(&byte))
}

/// 常量时间比较（防哈希逐字节探测；输入长度差异仍可见，但哈希定长）。
fn hashes_equal(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

// —— 注册表文件 ——

#[derive(Serialize, Deserialize)]
struct RegistryFile {
    format_version: u64,
    #[serde(rename = "entry", default)]
    entries: Vec<RegistryEntry>,
}

#[derive(Serialize, Deserialize)]
struct RegistryEntry {
    kind: String,
    id: String,
    token_hash: String,
}

/// 注册表内存形态：`"<kind>:<id>" → token_hash`。
type Entries = BTreeMap<String, String>;

fn entries_key(kind: PluginKind, id: &str) -> String {
    format!("{}:{id}", kind.as_str())
}

fn validate_id(value: &str) -> Result<(), String> {
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
        return Err(format!("插件 id 无效: {value:?}"));
    }
    Ok(())
}

fn validate_entry(entry: &RegistryEntry) -> Result<(), String> {
    validate_id(&entry.id)?;
    if PluginKind::from_str(&entry.kind).is_none() {
        return Err(format!("注册表条目 kind 无效: {}", entry.kind));
    }
    if entry.token_hash.len() != 64 || !entry.token_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("注册表条目 token_hash 不是 64 位十六进制".into());
    }
    Ok(())
}

/// 加载注册表；文件不存在 → 空表（空安装不制造状态）。
fn load_registry(path: &Path) -> Result<Entries, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("不可检查: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("类型无效（符号链接或非普通文件）".into());
    }
    check_owned_file(&metadata)?;
    let bytes = fs::read(path).map_err(|error| format!("不可读取: {error}"))?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(format!("超过 {MAX_REGISTRY_BYTES} 字节上限"));
    }
    let text = String::from_utf8(bytes).map_err(|_| "不是有效的 UTF-8".to_string())?;
    let file: RegistryFile = toml::from_str(&text).map_err(|error| format!("解析失败: {error}"))?;
    if file.format_version != REGISTRY_FORMAT_VERSION {
        return Err(format!(
            "未来 format_version {}（当前支持 {REGISTRY_FORMAT_VERSION}）",
            file.format_version
        ));
    }
    if file.entries.len() > MAX_ENTRIES {
        return Err(format!("条目数超过 {MAX_ENTRIES} 上限"));
    }
    let mut entries = BTreeMap::new();
    for entry in &file.entries {
        validate_entry(entry)?;
        let key = entries_key(
            PluginKind::from_str(&entry.kind).expect("validate_entry 已检查 kind"),
            &entry.id,
        );
        if entries.insert(key, entry.token_hash.clone()).is_some() {
            return Err(format!("重复条目: {}", entry.id));
        }
    }
    Ok(entries)
}

#[cfg(unix)]
fn check_owned_file(metadata: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid {
        return Err("必须由当前用户拥有".into());
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(format!("权限过宽: {:o}（要求 0600）", metadata.mode()));
    }
    if metadata.nlink() != 1 {
        return Err(format!("必须只有单个硬链接（当前 {}）", metadata.nlink()));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_owned_file(_metadata: &fs::Metadata) -> Result<(), String> {
    Ok(())
}

/// 原子发布：同目录临时文件（0600）→ fsync → rename → 父目录 fsync。
fn save_registry(path: &Path, entries: &Entries) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("注册表路径缺少父目录: {path:?}"))?;
    let file = RegistryFile {
        format_version: REGISTRY_FORMAT_VERSION,
        entries: entries
            .iter()
            .map(|(key, token_hash)| {
                let (kind, id) = key.split_once(':').expect("entries_key 恒含分隔符");
                RegistryEntry {
                    kind: kind.into(),
                    id: id.into(),
                    token_hash: token_hash.clone(),
                }
            })
            .collect(),
    };
    let body = toml::to_string_pretty(&file).map_err(|error| format!("序列化失败: {error}"))?;
    let temp = parent.join(format!(".{REGISTRY_FILE}.tmp-{}", generate_token()));
    let result = publish_atomically(&temp, path, body.as_bytes());
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn publish_atomically(temp: &Path, target: &Path, body: &[u8]) -> Result<(), String> {
    use std::io::Write as _;

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(temp)
            .map_err(|error| format!("临时文件创建失败: {error}"))?
    };
    #[cfg(not(unix))]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(|error| format!("临时文件创建失败: {error}"))?;
    file.write_all(body).map_err(|error| format!("临时文件写入失败: {error}"))?;
    file.sync_all().map_err(|error| format!("临时文件落盘失败: {error}"))?;
    drop(file);
    fs::rename(temp, target).map_err(|error| format!("原子发布失败: {error}"))?;
    #[cfg(unix)]
    {
        let dir = fs::File::open(
            target
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )
        .map_err(|error| format!("父目录打开失败: {error}"))?;
        dir.sync_all().map_err(|error| format!("父目录落盘失败: {error}"))?;
    }
    Ok(())
}

// —— 运行时缝 ——

enum SeamState {
    Ready(Entries),
    Unavailable,
}

/// 身份缝运行时句柄。以 `Arc` 提供进 cordis Context；无资源需卸载。
pub struct IdentitySeam {
    path: PathBuf,
    state: Mutex<SeamState>,
    /// 按主体字符串的越权拒绝计数（内存态，重启清零；ADR-0018 观测面）。
    denied: Mutex<BTreeMap<String, u64>>,
}

impl Service for IdentitySeam {
    fn name(&self) -> &'static str {
        "plugin.identity-seam"
    }
}

/// `verify` 的失败分类：`Unavailable` 映射 -32015，`Invalid` 映射 -32014。
#[derive(Debug, Eq, PartialEq)]
pub enum VerifyError {
    Unavailable,
    Invalid,
}

/// `rotate` / `reveal` 的失败分类。
#[derive(Debug, Eq, PartialEq)]
pub enum RotateError {
    Unavailable,
    /// 注册表中无该包条目：包未安装或已被移除（孤儿收敛后）。
    UnknownId,
}

/// 扫描对账报告。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReconcileReport {
    pub added: usize,
    pub removed: usize,
}

impl IdentitySeam {
    /// 打开注册表并加载。任何失败都进入 `Unavailable`（有界诊断一次），
    /// 绝不清理、绝不带病运行。
    pub fn open(registry_path: PathBuf) -> Arc<Self> {
        let state = match load_registry(&registry_path) {
            Ok(entries) => SeamState::Ready(entries),
            Err(reason) => {
                eprintln!(
                    "[maild/auth] {} 无法加载，插件身份缝停摆（supervised 插件不会启动，authenticate 返回 -32015）: {reason}",
                    registry_path.display()
                );
                SeamState::Unavailable
            }
        };
        Arc::new(Self {
            path: registry_path,
            state: Mutex::new(state),
            denied: Mutex::new(BTreeMap::new()),
        })
    }

    /// 注册表路径（诊断用）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_ready(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), SeamState::Ready(_))
    }

    /// 该包当前是否有身份条目。停摆时一律 `false`。
    pub fn contains(&self, kind: PluginKind, id: &str) -> bool {
        match &*self.state.lock().unwrap() {
            SeamState::Ready(entries) => entries.contains_key(&entries_key(kind, id)),
            SeamState::Unavailable => false,
        }
    }

    /// 用明文凭证换取主体。哈希比较是常量时间的。
    pub fn verify(&self, token: &str) -> Result<Principal, VerifyError> {
        if !token_well_formed(token) {
            // 格式错误不需要查表，但停摆优先：不向调用方泄露缝状态差异。
            return match &*self.state.lock().unwrap() {
                SeamState::Ready(_) => Err(VerifyError::Invalid),
                SeamState::Unavailable => Err(VerifyError::Unavailable),
            };
        }
        let candidate = hash_token(token);
        let state = self.state.lock().unwrap();
        let entries = match &*state {
            SeamState::Ready(entries) => entries,
            SeamState::Unavailable => return Err(VerifyError::Unavailable),
        };
        for (key, token_hash) in entries {
            if hashes_equal(&candidate, token_hash) {
                let (kind, id) = key.split_once(':').expect("entries_key 恒含分隔符");
                return Ok(match PluginKind::from_str(kind).expect("加载已校验 kind") {
                    PluginKind::External => Principal::Plugin(id.to_string()),
                    PluginKind::Frontend => Principal::Frontend(id.to_string()),
                });
            }
        }
        Err(VerifyError::Invalid)
    }

    /// 扫描对账：补发缺失条目（新随机凭证明文直接丢弃，首次 reveal/spawn 时
    /// 才会轮换成可用凭证）、删除包已消失的孤儿条目；有变化才发布一次。
    /// 解析失败（停摆）时绝不写入。
    pub fn reconcile(
        &self,
        kind: PluginKind,
        live_ids: &[&str],
    ) -> Result<ReconcileReport, RotateError> {
        let mut state = self.state.lock().unwrap();
        let entries = match &mut *state {
            SeamState::Ready(entries) => entries,
            SeamState::Unavailable => return Err(RotateError::Unavailable),
        };
        let mut report = ReconcileReport::default();
        for id in live_ids {
            if validate_id(id).is_err() {
                continue;
            }
            let key = entries_key(kind, id);
            // 明文即刻丢弃：条目存在只表示「身份可用」，凭证在首次轮换时产生。
            if let std::collections::btree_map::Entry::Vacant(slot) = entries.entry(key) {
                let discarded = generate_token();
                slot.insert(hash_token(&discarded));
                report.added += 1;
            }
        }
        let prefix = format!("{}:", kind.as_str());
        let dead: Vec<String> = entries
            .keys()
            .filter(|key| key.starts_with(&prefix) && !live_ids.contains(&&key[prefix.len()..]))
            .cloned()
            .collect();
        for key in &dead {
            entries.remove(key);
        }
        report.removed = dead.len();
        if report.added + report.removed > 0 {
            if let Err(reason) = save_registry(&self.path, entries) {
                eprintln!(
                    "[maild/auth] {} 发布失败，身份缝停摆: {reason}",
                    self.path.display()
                );
                let _ = std::mem::replace(&mut *state, SeamState::Unavailable);
                return Err(RotateError::Unavailable);
            }
        }
        Ok(report)
    }

    /// 为一次 spawn 轮换凭证（每 spawn 新 token，旧 token 对新连接立即失效）。
    /// 返回明文；条目必须已由扫描对账建立。
    pub fn rotate(&self, kind: PluginKind, id: &str) -> Result<String, RotateError> {
        self.rotate_inner(kind, id)
    }

    /// 用户显式 reveal：语义即轮换——每次生成新凭证、旧凭证对新连接失效、
    /// 明文只在本次应答出现。
    pub fn reveal(&self, kind: PluginKind, id: &str) -> Result<String, RotateError> {
        self.rotate_inner(kind, id)
    }

    fn rotate_inner(&self, kind: PluginKind, id: &str) -> Result<String, RotateError> {
        let mut state = self.state.lock().unwrap();
        let entries = match &mut *state {
            SeamState::Ready(entries) => entries,
            SeamState::Unavailable => return Err(RotateError::Unavailable),
        };
        let key = entries_key(kind, id);
        if !entries.contains_key(&key) {
            return Err(RotateError::UnknownId);
        }
        let token = generate_token();
        entries.insert(key, hash_token(&token));
        save_registry(&self.path, entries).map_err(|reason| {
            eprintln!("[maild/auth] {} 凭证轮换发布失败，身份缝停摆: {reason}", self.path.display());
            let _ = std::mem::replace(&mut *state, SeamState::Unavailable);
            RotateError::Unavailable
        })?;
        Ok(token)
    }

    /// 记录一次越权拒绝（主体字符串形如 `plugin:<id>`；脱敏，不含方法参数）。
    /// `user` 主体不存在越权，防御性跳过。
    pub fn record_denial(&self, principal: &str) {
        if principal == "user" {
            return;
        }
        *self.denied.lock().unwrap().entry(principal.to_owned()).or_insert(0) += 1;
    }

    /// 某主体的累计拒绝次数。
    pub fn denied_total(&self, principal: &str) -> u64 {
        self.denied
            .lock()
            .unwrap()
            .get(principal)
            .copied()
            .unwrap_or(0)
    }
}

/// 会话主体的共享句柄：serve 循环经认证写入，推送监听器在投递前读取
/// （ADR-0018 推送过滤）。克隆共享同一份状态。
#[derive(Clone, Default)]
pub struct SharedPrincipal(Arc<Mutex<Option<Principal>>>);

impl SharedPrincipal {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }

    /// 绑定/覆盖主体（仅认证路径调用）。
    pub fn set(&self, principal: Principal) {
        *self.0.lock().unwrap() = Some(principal);
    }

    /// 当前主体快照；未绑定即 `User`。
    pub fn get(&self) -> Principal {
        self.0.lock().unwrap().clone().unwrap_or(Principal::User)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    fn ready_seam(dir: &Path) -> Arc<IdentitySeam> {
        IdentitySeam::open(dir.join(REGISTRY_FILE))
    }

    #[cfg(unix)]
    fn secure_dir() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    #[cfg(not(unix))]
    fn secure_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn token_format_and_hash_roundtrip() {
        let token = generate_token();
        assert_eq!(token.len(), TOKEN_PREFIX.len() + TOKEN_BODY_LEN);
        assert!(token_well_formed(&token));
        assert!(!token_well_formed("mdp_short"));
        assert!(!token_well_formed("nope_0123456789012345678901234567890123456789012"));
        // base64url 字母表：+ / 不出现，- _ 出现（统计意义上有概率，这里只查非法字符）
        let hash = hash_token(&token);
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, hash_token(&token), "哈希稳定");
        assert_ne!(hash, hash_token(&generate_token()));
        assert!(hashes_equal(&hash, &hash));
        assert!(!hashes_equal(&hash, &hash_token("mdp_other")));
    }

    #[test]
    fn denial_counters_key_per_principal_and_ignore_user() {
        let dir = secure_dir();
        let seam = ready_seam(dir.path());
        assert_eq!(seam.denied_total("plugin:org.x"), 0);
        seam.record_denial("plugin:org.x");
        seam.record_denial("plugin:org.x");
        seam.record_denial("plugin:org.y");
        assert_eq!(seam.denied_total("plugin:org.x"), 2);
        assert_eq!(seam.denied_total("plugin:org.y"), 1);
        // user 主体不存在越权，防御性跳过。
        seam.record_denial("user");
        assert_eq!(seam.denied_total("user"), 0);
    }

    #[test]
    fn principal_and_kind_strings_are_stable() {
        assert_eq!(Principal::User.as_str(), "user");
        assert_eq!(Principal::Plugin("org.x".into()).as_str(), "plugin:org.x");
        assert_eq!(Principal::Frontend("org.y".into()).as_str(), "frontend:org.y");
        assert_eq!(PluginKind::from_str("plugin"), Some(PluginKind::External));
        assert_eq!(PluginKind::from_str("frontend"), Some(PluginKind::Frontend));
        assert_eq!(PluginKind::from_str("other"), None);
    }

    #[test]
    fn missing_registry_is_empty_and_ready() {
        let dir = secure_dir();
        let seam = ready_seam(dir.path());
        assert!(seam.is_ready());
        assert_eq!(seam.verify(&generate_token()), Err(VerifyError::Invalid));
    }

    #[test]
    fn reconcile_provisions_and_sweeps_orphans() {
        let dir = secure_dir();
        let seam = ready_seam(dir.path());
        let report = seam
            .reconcile(PluginKind::External, &["org.example.a", "org.example.b"])
            .unwrap();
        assert_eq!(report, ReconcileReport { added: 2, removed: 0 });
        assert!(seam.contains(PluginKind::External, "org.example.a"));
        // 重复对账幂等。
        let again = seam.reconcile(PluginKind::External, &["org.example.a"]).unwrap();
        assert_eq!(again, ReconcileReport { added: 0, removed: 1 });
        assert!(!seam.contains(PluginKind::External, "org.example.b"));
        // 前缀不串门：frontend 条目不受 external 对账影响。
        seam.reconcile(PluginKind::Frontend, &["org.example.f"]).unwrap();
        seam.reconcile(PluginKind::External, &[]).unwrap();
        assert!(seam.contains(PluginKind::Frontend, "org.example.f"));
    }

    #[test]
    fn rotate_enables_verify_and_revokes_previous() {
        let dir = secure_dir();
        let seam = ready_seam(dir.path());
        seam.reconcile(PluginKind::External, &["org.example.a"]).unwrap();
        // 未轮换前条目不可验证（发放时明文已丢弃）。
        assert_eq!(seam.verify(&generate_token()), Err(VerifyError::Invalid));
        let first = seam.rotate(PluginKind::External, "org.example.a").unwrap();
        assert_eq!(
            seam.verify(&first).unwrap(),
            Principal::Plugin("org.example.a".into())
        );
        let second = seam.reveal(PluginKind::External, "org.example.a").unwrap();
        assert_ne!(first, second);
        assert_eq!(seam.verify(&first), Err(VerifyError::Invalid), "旧凭证已吊销");
        assert_eq!(
            seam.verify(&second).unwrap(),
            Principal::Plugin("org.example.a".into())
        );
        // 未安装的包不能凭空造凭证。
        assert_eq!(
            seam.rotate(PluginKind::External, "org.example.ghost"),
            Err(RotateError::UnknownId)
        );
        // frontend 主体正确区分。
        seam.reconcile(PluginKind::Frontend, &["org.example.f"]).unwrap();
        let ftoken = seam.rotate(PluginKind::Frontend, "org.example.f").unwrap();
        assert_eq!(
            seam.verify(&ftoken).unwrap(),
            Principal::Frontend("org.example.f".into())
        );
    }

    #[test]
    fn registry_survives_reopen() {
        let dir = secure_dir();
        let seam = ready_seam(dir.path());
        seam.reconcile(PluginKind::External, &["org.example.a"]).unwrap();
        let token = seam.rotate(PluginKind::External, "org.example.a").unwrap();
        drop(seam);
        let reopened = ready_seam(dir.path());
        assert_eq!(
            reopened.verify(&token).unwrap(),
            Principal::Plugin("org.example.a".into())
        );
    }

    #[test]
    fn corrupt_registry_fails_closed_without_cleanup() {
        let dir = secure_dir();
        let path = dir.path().join(REGISTRY_FILE);
        fs::write(&path, "format_version = 99\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let seam = IdentitySeam::open(path.clone());
        assert!(!seam.is_ready());
        assert_eq!(seam.verify(&generate_token()), Err(VerifyError::Unavailable));
        assert_eq!(
            seam.rotate(PluginKind::External, "org.example.a"),
            Err(RotateError::Unavailable)
        );
        assert_eq!(
            seam.reconcile(PluginKind::External, &["org.a"]),
            Err(RotateError::Unavailable)
        );
        // 停摆绝不清理：坏文件原样保留。
        assert!(path.exists());
        assert!(fs::read_to_string(&path).unwrap().contains("format_version = 99"));
    }

    #[test]
    fn future_version_and_shape_violations_fail_closed() {
        let dir = secure_dir();
        let good = format!(
            "format_version = 1\n\n[[entry]]\nkind = \"plugin\"\nid = \"org.example.a\"\ntoken_hash = \"{}\"\n",
            "a".repeat(64)
        );
        let path = dir.path().join(REGISTRY_FILE);
        fs::write(&path, &good).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(ready_seam(dir.path()).is_ready());

        // 重复条目拒绝。
        let duplicated = format!("{good}{good}");
        fs::write(&path, duplicated).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!ready_seam(dir.path()).is_ready());

        // 坏哈希长度拒绝。
        fs::write(&path, "format_version = 1\n\n[[entry]]\nkind = \"plugin\"\nid = \"org.a\"\ntoken_hash = \"zz\"\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!ready_seam(dir.path()).is_ready());
    }
}
