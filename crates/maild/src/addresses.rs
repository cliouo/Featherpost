//! 地址层（ADR-0022，M4-06）：别名与一次性地址。
//!
//! 地址是父账户的投递别名，不新增传输资源：收信按 To 头标记归属
//! （`recipient` 列），发信仅重写 From、凭据/规则/审计完全沿用父账户。
//! 过期是惰性的——读取与使用时判定，无常驻定时器（headless 资源观）。

use serde::{Deserialize, Serialize};

/// 单账户地址上限。工程护栏而非产品语义：防止异常客户端无限写入；
/// 对「无限别名」场景实际不可触达（上游 catch-all 域名数才是真边界）。
pub const MAX_ADDRESSES_PER_ACCOUNT: usize = 1024;

/// disposable 随机 local part 的字符数（base32 小写，约 60 bit 熵）。
const DISPOSABLE_LOCAL_PART_CHARS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressKind {
    /// 用户显式命名的别名。
    Alias,
    /// daemon 随机生成的一次性地址。
    Disposable,
}

impl AddressKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::Disposable => "disposable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "alias" => Some(Self::Alias),
            "disposable" => Some(Self::Disposable),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressState {
    Active,
    Disabled,
    Expired,
}

impl AddressState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
            Self::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "disabled" => Some(Self::Disabled),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// `addresses` 表的一行。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddressRecord {
    pub id: String,
    /// 父账户 id。
    pub account: String,
    /// 完整地址（local@domain），唯一。
    pub address: String,
    pub kind: AddressKind,
    pub label: String,
    pub state: AddressState,
    /// 创建时间（unix 秒）。
    pub created_at: u64,
    /// 到期时间（unix 秒）；None = 永久。
    pub expires_at: Option<u64>,
}

/// RPC 视图（服务端序列化形态）。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AddressView {
    pub id: String,
    pub account: String,
    pub address: String,
    pub kind: &'static str,
    pub label: String,
    pub state: &'static str,
    pub created_at: u64,
    pub expires_at: Option<u64>,
}

impl AddressRecord {
    pub fn into_view(self) -> AddressView {
        AddressView {
            id: self.id,
            account: self.account,
            address: self.address,
            kind: self.kind.as_str(),
            label: self.label,
            state: self.state.as_str(),
            created_at: self.created_at,
            expires_at: self.expires_at,
        }
    }

    /// 惰性过期判定：active 且已过到期时间 → 视为 expired。
    pub fn effective_state(&self, now_unix_secs: u64) -> AddressState {
        match (self.state, self.expires_at) {
            (state, Some(expires_at))
                if state != AddressState::Expired && expires_at <= now_unix_secs =>
            {
                AddressState::Expired
            }
            (state, _) => state,
        }
    }
}

/// 地址语法校验：≤254 字节、恰好一个 @、local/domain 非空且无空白与
/// 控制字符、domain 至少一个点、不得以点或连字符开头结尾（工程级
/// 语法门；不做 RFC 5321 全量引号/local-part 语义）。
pub fn validate_address(address: &str) -> Result<(), String> {
    if address.is_empty() {
        return Err("地址不能为空".into());
    }
    if address.len() > 254 {
        return Err("地址过长（上限 254 字节）".into());
    }
    let Some((local, domain)) = address.split_once('@') else {
        return Err("地址必须包含一个 @".into());
    };
    if local.is_empty() || domain.is_empty() {
        return Err("@ 两侧都不能为空".into());
    }
    if address.matches('@').count() != 1 {
        return Err("地址只能包含一个 @".into());
    }
    for part in [local, domain] {
        if part.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
            return Err("地址不能包含空白或控制字符".into());
        }
    }
    if local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return Err("local part 的点位置不合法".into());
    }
    let domain_shape_ok = domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.starts_with('-')
        && !domain.ends_with('-')
        && !domain.contains("..");
    if !domain_shape_ok {
        return Err("域名形状不合法".into());
    }
    Ok(())
}

/// 从父账户地址提取域名（生成 disposable 用）。父账户地址必然通过过
/// 账户形状校验；此处防御性兜底。
pub fn domain_of(address: &str) -> Option<&str> {
    address.rsplit_once('@').map(|(_, domain)| domain)
}

/// 生成 disposable local part：小写 base32、12 字符（约 60 bit 熵），
/// 密码学 RNG（OS 系统随机源），冲突由调用方重试。
pub fn generate_disposable_local_part() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut random = [0u8; DISPOSABLE_LOCAL_PART_CHARS];
    getrandom::fill(&mut random).expect("系统随机源不可用");
    random
        .iter()
        .map(|byte| (ALPHABET[(*byte & 31) as usize] as char).to_string())
        .collect()
}

/// 生成新地址行 id（对齐账户 id 形状：addr_ + 26 位 base32）。
pub fn generate_address_id() -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut random = [0u8; 26];
    getrandom::fill(&mut random).expect("系统随机源不可用");
    let mut id = String::with_capacity(31);
    id.push_str("addr_");
    for byte in random {
        id.push(ALPHABET[(byte & 31) as usize] as char);
    }
    id
}

/// 仅用于形状校验的占位地址行 id（满足 addr_ + 26 位 base32 约束）。
pub fn placeholder_address_id() -> String {
    concat!("addr_", "aaaaaaaaaaaaaaaaaaaaaaaaaa").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_grammar_accepts_normal_and_rejects_malformed() {
        assert!(validate_address("shop12@example.com").is_ok());
        assert!(validate_address("a.b@sub.example.test").is_ok());
        assert!(validate_address("").is_err());
        assert!(validate_address("no-at-sign").is_err());
        assert!(validate_address("a@@b.com").is_err());
        assert!(validate_address("@example.com").is_err());
        assert!(validate_address("local@").is_err());
        assert!(validate_address(".lead@example.com").is_err());
        assert!(validate_address("trail.@example.com").is_err());
        assert!(validate_address("do..uble@example.com").is_err());
        assert!(validate_address("local@nodot").is_err());
        assert!(validate_address("local@.dot.com").is_err());
        assert!(validate_address("local@-dash.com").is_err());
        assert!(validate_address("sp ace@example.com").is_err());
        assert!(validate_address("ctrl\u{7}@example.com").is_err());
        assert!(validate_address(&format!("{}@example.com", "a".repeat(250))).is_err());
    }

    #[test]
    fn disposable_local_part_shape_and_entropy() {
        for _ in 0..64 {
            let part = generate_disposable_local_part();
            assert_eq!(part.len(), 12);
            assert!(
                part.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()),
                "必须是 base32 小写字母表: {part}"
            );
        }
        // 两个样本相等的概率 ~2^-60；连续不等即可视为熵正常。
        assert_ne!(
            generate_disposable_local_part(),
            generate_disposable_local_part()
        );
    }

    #[test]
    fn effective_state_is_lazy_over_expiry() {
        let record = |state: AddressState, expires_at: Option<u64>| AddressRecord {
            id: placeholder_address_id(),
            account: concat!("acct_", "aaaaaaaaaaaaaaaaaaaaaaaaaa").to_owned(),
            address: "x@example.test".into(),
            kind: AddressKind::Disposable,
            label: String::new(),
            state,
            created_at: 100,
            expires_at,
        };
        assert_eq!(
            record(AddressState::Active, Some(500)).effective_state(499),
            AddressState::Active
        );
        assert_eq!(
            record(AddressState::Active, Some(500)).effective_state(500),
            AddressState::Expired
        );
        assert_eq!(
            record(AddressState::Active, None).effective_state(u64::MAX),
            AddressState::Active
        );
        // TTL 是硬上限：disabled 地址到点后同样不可再次启用。
        assert_eq!(
            record(AddressState::Disabled, Some(10)).effective_state(1000),
            AddressState::Expired
        );
    }

    #[test]
    fn kind_and_state_round_trip_parse() {
        assert_eq!(AddressKind::parse("alias"), Some(AddressKind::Alias));
        assert_eq!(
            AddressKind::parse("disposable"),
            Some(AddressKind::Disposable)
        );
        assert_eq!(AddressKind::parse("other"), None);
        assert_eq!(AddressState::parse("active"), Some(AddressState::Active));
        assert_eq!(AddressState::parse("expired"), Some(AddressState::Expired));
        assert_eq!(AddressState::parse("Active"), None);
        assert_eq!(AddressKind::Alias.as_str(), "alias");
        assert_eq!(AddressState::Expired.as_str(), "expired");
    }
}
