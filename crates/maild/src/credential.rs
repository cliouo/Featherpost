//! credential：凭据存储缝（ADR-0015 §5）。
//!
//! - macOS 生产 provider：Security framework 的 generic password item，
//!   service 固定为 [`account_catalog::CREDENTIAL_SERVICE`]，不 shell out `security`；
//! - hermetic 测试 provider：内存实现 + 可注入失败点；
//! - 其余平台：返回 unsupported，等待独立阶段。
//!
//! 安全承诺是「不持久化、不长期驻留、尽快清零」：秘密只在认证前取入
//! [`SecretBuffer`]，离开作用域即清零；错误只暴露稳定分类与 OSStatus，
//! 不回传 Keychain localized description。

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use zeroize::{Zeroize, Zeroizing};

#[cfg(target_os = "macos")]
use crate::account_catalog::CREDENTIAL_SERVICE;
use crate::account_catalog::CredentialKey;

/// 短生命周期秘密缓冲：离开作用域即清零，Debug/Display 永不回显内容。
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretBuffer(Zeroizing<Vec<u8>>);

impl SecretBuffer {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[secret {} bytes]", self.0.len())
    }
}

/// 清零辅助：显式擦除可变缓冲（协议库边界用）。
pub fn wipe(buffer: &mut [u8]) {
    buffer.zeroize();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialErrorKind {
    /// 平台不支持凭据库（如 Windows provider 未落地）。
    Unsupported,
    /// 凭据库不可用：锁定、需要交互或系统服务失败。
    Unavailable,
    /// 目标 item 不存在。
    NotFound,
    /// 输入超出平台限制。
    InvalidInput,
    /// 平台错误（附 OSStatus / 原始码）。
    Os,
}

/// 稳定分类错误：对外只暴露 kind、原始码与固定文案，不含平台描述文本。
#[derive(Debug)]
pub struct CredentialError {
    pub kind: CredentialErrorKind,
    /// 平台原始状态码（macOS 为 OSStatus），仅用于诊断。
    pub status: Option<i32>,
    message: String,
}

impl CredentialError {
    fn new(kind: CredentialErrorKind, status: Option<i32>, message: impl Into<String>) -> Self {
        Self {
            kind,
            status,
            message: message.into(),
        }
    }

    pub fn unsupported(platform: &str) -> Self {
        Self::new(
            CredentialErrorKind::Unsupported,
            None,
            format!("{platform} 凭据 provider 尚未落地"),
        )
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(CredentialErrorKind::InvalidInput, None, message)
    }

    pub fn kind(&self) -> CredentialErrorKind {
        self.kind
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(f, "{}（status {status}）", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for CredentialError {}

// macOS OSStatus 子集（稳定分类映射用）。
#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
#[cfg(target_os = "macos")]
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
#[cfg(target_os = "macos")]
const ERR_SEC_NOT_AVAILABLE: i32 = -25291;

#[cfg(target_os = "macos")]
fn map_security_framework_error(error: security_framework::base::Error) -> CredentialError {
    let code = error.code();
    let kind = match code {
        ERR_SEC_ITEM_NOT_FOUND => CredentialErrorKind::NotFound,
        ERR_SEC_INTERACTION_NOT_ALLOWED | ERR_SEC_NOT_AVAILABLE => {
            CredentialErrorKind::Unavailable
        }
        _ => CredentialErrorKind::Os,
    };
    CredentialError::new(kind, Some(code), "Keychain 操作失败")
}

/// 凭据存储 spine service。实现必须线程安全且无 panicking 路径。
pub trait CredentialStore: Send + Sync {
    /// 创建或更新一个 item。
    fn save(&self, key: &CredentialKey, secret: &[u8]) -> Result<(), CredentialError>;

    /// 读取一个 item 到短生命周期缓冲。
    fn load(&self, key: &CredentialKey) -> Result<SecretBuffer, CredentialError>;

    /// 删除一个 item；返回是否确实存在过（不存在不算错误）。
    fn delete(&self, key: &CredentialKey) -> Result<bool, CredentialError>;

    /// 枚举本 service 下的全部 account 属性（孤儿收敛用）。
    /// 返回原始字符串，由调用方按命名约定过滤，避免误删无法解析的遗留项。
    fn list_attrs(&self) -> Result<Vec<String>, CredentialError>;

    /// 存在性检查：不取回秘密内容到调用方（实现上允许短暂读取后丢弃）。
    fn contains(&self, key: &CredentialKey) -> Result<bool, CredentialError> {
        match self.load(key) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == CredentialErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

// —— hermetic 内存 provider ——

/// 内存实现：单测与事务故障注入用。键即 [`CredentialKey::attr_account`]。
#[derive(Default)]
pub struct MemoryCredentialStore {
    items: Mutex<HashMap<String, Vec<u8>>>,
    fail_saves: AtomicBool,
}

impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入保存失败点（模拟 Keychain 写入失败的事务回滚路径）。
    pub fn set_save_failure(&self, enabled: bool) {
        self.fail_saves.store(enabled, Ordering::SeqCst);
    }

    fn attr(key: &CredentialKey) -> String {
        key.attr_account()
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn save(&self, key: &CredentialKey, secret: &[u8]) -> Result<(), CredentialError> {
        if secret.len() > crate::account_catalog::MAX_SECRET_BYTES {
            return Err(CredentialError::invalid(format!(
                "秘密超过 {} 字节上限",
                crate::account_catalog::MAX_SECRET_BYTES
            )));
        }
        if self.fail_saves.load(Ordering::SeqCst) {
            return Err(CredentialError::new(
                CredentialErrorKind::Unavailable,
                None,
                "注入的保存失败",
            ));
        }
        self.items
            .lock()
            .unwrap()
            .insert(Self::attr(key), secret.to_vec());
        Ok(())
    }

    fn load(&self, key: &CredentialKey) -> Result<SecretBuffer, CredentialError> {
        let attr = Self::attr(key);
        self.items
            .lock()
            .unwrap()
            .get(&attr)
            .map(|bytes| SecretBuffer::new(bytes.clone()))
            .ok_or_else(|| {
                // 内存 provider 仅用于测试；attr 非秘密，可安全进入诊断信息。
                CredentialError::new(
                    CredentialErrorKind::NotFound,
                    None,
                    format!("凭据 item 不存在: {attr}"),
                )
            })
    }

    fn delete(&self, key: &CredentialKey) -> Result<bool, CredentialError> {
        Ok(self.items.lock().unwrap().remove(&Self::attr(key)).is_some())
    }

    fn list_attrs(&self) -> Result<Vec<String>, CredentialError> {
        Ok(self.items.lock().unwrap().keys().cloned().collect())
    }
}

// —— macOS Keychain provider ——

/// macOS 生产 provider：generic password item，经 Security framework 直接调用。
#[cfg(target_os = "macos")]
pub struct MacosKeychainStore;

#[cfg(target_os = "macos")]
impl MacosKeychainStore {
    /// item 的 label 只含产品与账户展示名，不含服务器错误或秘密。
    fn label(account_display: &str) -> String {
        let mut label = String::from(maild_brand::PRODUCT_NAME);
        if !account_display.is_empty() {
            label.push_str(" · ");
            // 展示名限长并去控制字符，防 Keychain 访问列表出现怪异内容。
            for character in account_display.chars().take(64) {
                if character.is_control() {
                    break;
                }
                label.push(character);
            }
        }
        label
    }
}

#[cfg(target_os = "macos")]
impl CredentialStore for MacosKeychainStore {
    fn save(&self, key: &CredentialKey, secret: &[u8]) -> Result<(), CredentialError> {
        if secret.is_empty() || secret.len() > crate::account_catalog::MAX_SECRET_BYTES {
            return Err(CredentialError::invalid(format!(
                "秘密长度必须在 1..={} 字节",
                crate::account_catalog::MAX_SECRET_BYTES
            )));
        }
        // label 只含产品名（展示名在 store 层不可得），绝不含服务器错误或秘密。
        let mut options =
            security_framework::passwords_options::PasswordOptions::new_generic_password(
                CREDENTIAL_SERVICE,
                &key.attr_account(),
            );
        options.set_label(&Self::label(""));
        security_framework::passwords::set_generic_password_options(secret, options)
            .map_err(map_security_framework_error)
    }

    fn load(&self, key: &CredentialKey) -> Result<SecretBuffer, CredentialError> {
        security_framework::passwords::get_generic_password(
            CREDENTIAL_SERVICE,
            &key.attr_account(),
        )
        .map(SecretBuffer::new)
        .map_err(map_security_framework_error)
    }

    fn delete(&self, key: &CredentialKey) -> Result<bool, CredentialError> {
        match security_framework::passwords::delete_generic_password(
            CREDENTIAL_SERVICE,
            &key.attr_account(),
        ) {
            Ok(()) => Ok(true),
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(false),
            Err(error) => Err(map_security_framework_error(error)),
        }
    }

    fn list_attrs(&self) -> Result<Vec<String>, CredentialError> {
        use security_framework::item::{ItemClass, ItemSearchOptions, SearchResult};

        let mut options = ItemSearchOptions::new();
        options
            .class(ItemClass::generic_password())
            .service(CREDENTIAL_SERVICE)
            .load_attributes(true)
            .load_data(false)
            .load_refs(false);
        let results = match options.search() {
            Ok(results) => results,
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => return Ok(Vec::new()),
            Err(error) => return Err(map_security_framework_error(error)),
        };
        let mut attrs = Vec::with_capacity(results.len());
        for result in results {
            if let SearchResult::Dict(dict) = result {
                if let Some(fields) = SearchResult::Dict(dict).simplify_dict() {
                    if let Some(attr) = fields.get("acct") {
                        attrs.push(attr.clone());
                    }
                }
            }
        }
        Ok(attrs)
    }
}

// —— 其余平台 ——

/// 占位 provider：所有操作返回 unsupported（fail-closed）。
pub struct UnsupportedCredentialStore;

impl CredentialStore for UnsupportedCredentialStore {
    fn save(&self, _key: &CredentialKey, _secret: &[u8]) -> Result<(), CredentialError> {
        Err(CredentialError::unsupported("当前平台"))
    }

    fn load(&self, _key: &CredentialKey) -> Result<SecretBuffer, CredentialError> {
        Err(CredentialError::unsupported("当前平台"))
    }

    fn delete(&self, _key: &CredentialKey) -> Result<bool, CredentialError> {
        Err(CredentialError::unsupported("当前平台"))
    }

    fn list_attrs(&self) -> Result<Vec<String>, CredentialError> {
        Err(CredentialError::unsupported("当前平台"))
    }
}

/// 平台默认 provider 工厂。
pub fn platform_store() -> std::sync::Arc<dyn CredentialStore> {
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(MacosKeychainStore)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::sync::Arc::new(UnsupportedCredentialStore)
    }
}

/// Agent 装配使用的 provider 工厂。
///
/// 测试缝：`MAILD_TEST_CREDENTIAL_STORE=memory` 时改用按目录共享的内存实现，
/// 让 e2e 可以完全 hermetic 地验证事务、生命周期与跨重启持久性。
/// 该开关只在 debug 构建生效；release（Preview/正式 DMG）一律走平台凭据库。
/// 同一目录（同一份 accounts.toml 的宿主目录）在进程内共享同一实例，
/// 模拟「同一用户同一台机器」的真实语义；不同目录互不可见。
pub fn platform_store_for(catalog_path: &std::path::Path) -> std::sync::Arc<dyn CredentialStore> {
    if test_memory_store_requested() {
        use std::collections::HashMap;
        use std::sync::{Arc as StdArc, Mutex as StdMutex, OnceLock};

        static SHARED: OnceLock<StdMutex<HashMap<String, StdArc<MemoryCredentialStore>>>> =
            OnceLock::new();
        let map = SHARED.get_or_init(|| StdMutex::new(HashMap::new()));
        let scope = catalog_path
            .parent()
            .unwrap_or(std::path::Path::new(""))
            .to_string_lossy()
            .into_owned();
        let mut guard = map.lock().unwrap();
        let store = guard
            .entry(scope)
            .or_insert_with(|| StdArc::new(MemoryCredentialStore::new()))
            .clone();
        return store;
    }
    platform_store()
}

/// debug 构建专用的 hermetic 开关；release 恒为 false。
pub fn test_memory_store_requested() -> bool {
    cfg!(debug_assertions)
        && std::env::var_os("MAILD_TEST_CREDENTIAL_STORE").as_deref()
            == Some(std::ffi::OsStr::new("memory"))
}

/// debug 构建专用：允许 hermetic 自签证书（ADR-0015 §2 的测试豁免通道）。
/// release 恒为 false，受管 profile 本身也不携带任何自签开关。
pub fn test_accept_invalid_certs() -> bool {
    cfg!(debug_assertions)
        && std::env::var_os("MAILD_TEST_ACCEPT_INVALID_CERTS").as_deref()
            == Some(std::ffi::OsStr::new("1"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_catalog::{generate_account_id, CredentialRole};

    fn key(revision: u32) -> CredentialKey {
        CredentialKey::new(generate_account_id(), CredentialRole::Incoming, revision)
    }

    #[test]
    fn memory_store_supports_add_get_update_delete_and_list() {
        let store = MemoryCredentialStore::new();
        let k = key(1);

        assert!(matches!(
            store.load(&k).unwrap_err().kind(),
            CredentialErrorKind::NotFound
        ));

        store.save(&k, b"first").unwrap();
        assert_eq!(store.load(&k).unwrap().expose(), b"first");

        // set_generic_password 语义：同键覆盖更新。
        store.save(&k, b"second").unwrap();
        assert_eq!(store.load(&k).unwrap().expose(), b"second");

        let attrs = store.list_attrs().unwrap();
        assert_eq!(attrs, vec![k.attr_account()]);

        assert!(store.delete(&k).unwrap());
        assert!(!store.delete(&k).unwrap(), "重复删除不是错误但返回 false");
        assert!(matches!(
            store.load(&k).unwrap_err().kind(),
            CredentialErrorKind::NotFound
        ));
    }

    #[test]
    fn injected_save_failures_surface_as_unavailable() {
        let store = MemoryCredentialStore::new();
        store.set_save_failure(true);
        let error = store.save(&key(1), b"x").unwrap_err();
        assert_eq!(error.kind(), CredentialErrorKind::Unavailable);
        store.set_save_failure(false);
        assert!(store.save(&key(1), b"x").is_ok());
    }

    #[test]
    fn oversized_secrets_are_rejected_before_touching_storage() {
        let store = MemoryCredentialStore::new();
        let error = store
            .save(&key(1), &vec![0u8; crate::account_catalog::MAX_SECRET_BYTES + 1])
            .unwrap_err();
        assert_eq!(error.kind(), CredentialErrorKind::InvalidInput);
    }

    #[test]
    fn secret_buffers_never_leak_through_debug_or_display() {
        let secret = SecretBuffer::new(b"hunter2".to_vec());
        let debug = format!("{secret:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert_eq!(secret.expose(), b"hunter2");

        // wipe() 对可变切片生效（协议库边界清零用）。
        let mut owned = vec![9u8; 8];
        wipe(&mut owned);
        assert!(owned.iter().all(|b| *b == 0));
    }

    #[cfg(target_os = "macos")]
    mod macos_real_keychain {
        use super::*;

        /// 真实 Keychain 冒烟测试：写入 → 读回 → 更新 → 删除 → 枚举收敛。
        /// 会写入当前用户 login keychain，因此保持 #[ignore]，真机验收时显式运行：
        /// `cargo test -p maild --lib credential -- --ignored`
        #[test]
        #[ignore = "touches the real user login keychain"]
        fn keychain_roundtrip_with_cleanup() {
            let store = MacosKeychainStore;
            let k = CredentialKey::new(generate_account_id(), CredentialRole::Outgoing, 42);

            store.save(&k, b"probe-secret").unwrap();
            assert_eq!(store.load(&k).unwrap().expose(), b"probe-secret");
            store.save(&k, b"rotated").unwrap();
            assert_eq!(store.load(&k).unwrap().expose(), b"rotated");
            assert!(store.list_attrs().unwrap().contains(&k.attr_account()));
            assert!(store.delete(&k).unwrap());
            assert!(!store.delete(&k).unwrap());
            assert!(matches!(
                store.load(&k).unwrap_err().kind(),
                CredentialErrorKind::NotFound
            ));
        }

        #[test]
        fn label_is_sanitized_and_bounded() {
            let label = MacosKeychainStore::label("Work\u{0}\u{1b}Account-very-long-name-that-keeps-going-and-going-and-going-and-going");
            assert!(label.starts_with(&format!("{} · Work", maild_brand::PRODUCT_NAME)));
            assert!(label.len() < 96);
            assert!(!label.contains('\u{1b}'));
        }
    }
}
