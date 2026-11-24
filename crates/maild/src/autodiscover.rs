//! autodiscover：邮箱服务商智能发现（ADR-0042）。
//!
//! 供 API / CLI / Tauri 添加账户向导复用的零外部依赖识别引擎：
//!
//! 1. [`detect_from_address`]：纯本地、零网络——按邮箱地址或域名匹配内置服务商知识库
//!    （含别名域），直接给出收发服务器、用户名格式、授权方式与中文引导；
//! 2. [`discover_provider`]：先走本地匹配；未命中且域名语法合法时，可选在 2.5s 总预算内
//!    并发探测 `imap.<domain>` / `mail.<domain>` / `smtp.<domain>` 的公网 TCP 可达性，
//!    否则返回干净的启发式猜测。
//!
//! SSRF 边界：只接受语法合法的公网域名（拒绝 IP 字面量、`localhost`、`.local`、`.internal`
//! 等内网后缀与单标签主机名）；网络探测先解析 DNS，**任一解析结果落在 loopback / RFC1918 /
//! link-local / ULA / unspecified 等非公网地址即放弃该主机**，且只连接已校验的那个地址
//! （不二次解析）。本模块绝不发送应用层数据，只做 TCP 三次握手后立即断开。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use maild_proxy::{
    ConnectOptions, ProxyConnector, ProxyProtocol, ProxyTarget, ResolvedRoute, TargetScheme,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;

use crate::account_catalog::SecurityMode;

/// 网络探测的总预算。
const NETWORK_BUDGET: Duration = Duration::from_millis(2500);
/// 推荐的收件文件夹。
const DEFAULT_MAILBOX: &str = "INBOX";

/// 该服务商在第三方客户端里应填哪类秘密。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthGuideKind {
    /// 应用专用密码（Gmail / iCloud / Fastmail / Yahoo 等，通常需先开两步验证）。
    AppPassword,
    /// 客户端授权码（QQ / 网易 / 新浪等国内邮箱）。
    AuthorizationCode,
    /// 直接使用账户密码（企业邮箱等，开启安全登录后可能改为专用密码）。
    AccountPassword,
    /// 本地桥接程序（Proton Mail Bridge）生成的密码。
    LocalBridge,
    /// 服务商已停用基础密码认证、需要 OAuth2（当前诚实提示）。
    #[serde(rename = "oauth2_required_notice")]
    OAuth2RequiredNotice,
}

/// 登录用户名的格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsernameFormat {
    /// 完整邮箱地址（`alice@example.com`）。
    FullAddress,
    /// 仅 `@` 之前的部分（`alice`）。
    LocalPart,
}

/// 一个发现出的服务器端点。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredEndpoint {
    pub server: String,
    pub port: u16,
    pub security: SecurityMode,
}

/// 一次发现的结果（亦是预设卡片的数据形态）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDiscovery {
    /// 稳定 ID，如 `gmail` / `qq` / `netease_163` / `custom`。
    pub provider_id: String,
    pub display_name: String,
    /// `preset` | `srv` | `tcp_probe` | `heuristic`。
    pub source: String,
    /// `verified` | `high` | `guessed`。
    pub confidence: String,
    /// IMAP 收件端点。
    pub incoming: DiscoveredEndpoint,
    /// 推荐收件文件夹（`INBOX`）。
    pub mailbox: String,
    /// 推荐启用 IDLE 推送。
    pub idle: bool,
    /// SMTP 发件端点。
    pub outgoing: DiscoveredEndpoint,
    pub username_format: UsernameFormat,
    /// 按 `username_format` 从输入地址推出的用户名；输入只有域名时为空串。
    pub suggested_username: String,
    pub auth_kind: AuthGuideKind,
    /// 该服务商分段展示应用密码（如 `abcd efgh ijkl mnop`），粘贴时应去掉空白。
    pub strip_secret_spaces: bool,
    /// 简明中文引导（如何开启 IMAP、获取授权码等）。
    pub notes: String,
    /// 官方设置帮助页（HTTPS）。
    pub help_url: Option<String>,
}

// —— 内置知识库 ——

struct Preset {
    id: &'static str,
    display_name: &'static str,
    incoming: (&'static str, u16, SecurityMode),
    outgoing: (&'static str, u16, SecurityMode),
    username_format: UsernameFormat,
    auth_kind: AuthGuideKind,
    strip_secret_spaces: bool,
    notes: &'static str,
    help_url: Option<&'static str>,
    /// 精确匹配的邮箱域名（小写）。为空表示只作为预设卡片（企业邮箱使用自有域名）。
    domains: &'static [&'static str],
}

use AuthGuideKind::*;
use SecurityMode::{Implicit as Tls, StartTls};
use UsernameFormat::FullAddress;

const PRESETS: &[Preset] = &[
    Preset {
        id: "gmail",
        display_name: "Gmail",
        incoming: ("imap.gmail.com", 993, Tls),
        outgoing: ("smtp.gmail.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AppPassword,
        strip_secret_spaces: true,
        notes: "需先为 Google 账号开启两步验证，再在「Google 账号 → 安全性 → 应用专用密码」生成 16 位密码填入；不能使用网页登录密码。",
        help_url: Some("https://support.google.com/mail/answer/185833"),
        domains: &["gmail.com", "googlemail.com"],
    },
    Preset {
        id: "icloud",
        display_name: "iCloud 邮件",
        incoming: ("imap.mail.me.com", 993, Tls),
        outgoing: ("smtp.mail.me.com", 587, StartTls),
        username_format: FullAddress,
        auth_kind: AppPassword,
        strip_secret_spaces: true,
        notes: "需开启 Apple 账户双重认证，在 account.apple.com「登录与安全 → App 专用密码」生成密码填入；发信仅支持 587 端口 STARTTLS。",
        help_url: Some("https://support.apple.com/102525"),
        domains: &["icloud.com", "me.com", "mac.com"],
    },
    Preset {
        id: "qq",
        display_name: "QQ 邮箱",
        incoming: ("imap.qq.com", 993, Tls),
        outgoing: ("smtp.qq.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: true,
        notes: "在 QQ 邮箱网页版「设置 → 账号 → POP3/IMAP/SMTP 服务」开启 IMAP/SMTP，并按提示生成授权码填入；不能使用 QQ 密码。",
        help_url: Some("https://service.mail.qq.com/"),
        domains: &["qq.com", "foxmail.com", "vip.qq.com"],
    },
    Preset {
        id: "netease_163",
        display_name: "网易 163 邮箱",
        incoming: ("imap.163.com", 993, Tls),
        outgoing: ("smtp.163.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → POP3/SMTP/IMAP」开启 IMAP/SMTP 服务，按提示获取客户端授权码填入；不能使用网页登录密码。",
        help_url: Some("https://help.mail.163.com/"),
        domains: &["163.com"],
    },
    Preset {
        id: "netease_163",
        display_name: "网易 VIP 163 邮箱",
        incoming: ("imap.vip.163.com", 993, Tls),
        outgoing: ("smtp.vip.163.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → POP3/SMTP/IMAP」开启 IMAP/SMTP 服务，按提示获取客户端授权码填入；不能使用网页登录密码。",
        help_url: Some("https://help.mail.163.com/"),
        domains: &["vip.163.com"],
    },
    Preset {
        id: "netease_163",
        display_name: "网易 188 邮箱",
        incoming: ("imap.188.com", 993, Tls),
        outgoing: ("smtp.188.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → POP3/SMTP/IMAP」开启 IMAP/SMTP 服务，按提示获取客户端授权码填入；不能使用网页登录密码。",
        help_url: Some("https://help.mail.163.com/"),
        domains: &["188.com"],
    },
    Preset {
        id: "netease_126",
        display_name: "网易 126 邮箱",
        incoming: ("imap.126.com", 993, Tls),
        outgoing: ("smtp.126.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → POP3/SMTP/IMAP」开启 IMAP/SMTP 服务，按提示获取客户端授权码填入；不能使用网页登录密码。",
        help_url: Some("https://help.mail.163.com/"),
        domains: &["126.com"],
    },
    Preset {
        id: "netease_yeah",
        display_name: "网易 yeah.net 邮箱",
        incoming: ("imap.yeah.net", 993, Tls),
        outgoing: ("smtp.yeah.net", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → POP3/SMTP/IMAP」开启 IMAP/SMTP 服务，按提示获取客户端授权码填入；不能使用网页登录密码。",
        help_url: Some("https://help.mail.163.com/"),
        domains: &["yeah.net"],
    },
    Preset {
        id: "tencent_exmail",
        display_name: "腾讯企业邮箱",
        incoming: ("imap.exmail.qq.com", 993, Tls),
        outgoing: ("smtp.exmail.qq.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AccountPassword,
        strip_secret_spaces: false,
        notes: "用户名为完整企业邮箱地址。若管理员开启了安全登录或微信动态密码，需在网页版「设置 → 客户端设置」生成客户端专用密码填入。",
        help_url: Some("https://exmail.qq.com/"),
        domains: &["exmail.qq.com"],
    },
    Preset {
        id: "aliyun",
        display_name: "阿里邮箱（个人版）",
        incoming: ("imap.aliyun.com", 993, Tls),
        outgoing: ("smtp.aliyun.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AccountPassword,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → 账户 → POP3/SMTP 及 IMAP/SMTP」开启 IMAP/SMTP；若开启了安全登录，需改用三方客户端专用密码。",
        help_url: Some("https://mail.aliyun.com/"),
        domains: &["aliyun.com"],
    },
    Preset {
        id: "aliyun_qiye",
        display_name: "阿里企业邮箱",
        incoming: ("imap.qiye.aliyun.com", 993, Tls),
        outgoing: ("smtp.qiye.aliyun.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AccountPassword,
        strip_secret_spaces: false,
        notes: "用户名为完整企业邮箱地址；若管理员开启了三方客户端安全密码，需在网页版设置中生成后填入。",
        help_url: Some("https://qiye.aliyun.com/"),
        domains: &["qiye.aliyun.com"],
    },
    Preset {
        id: "outlook",
        display_name: "Outlook / Hotmail",
        incoming: ("outlook.office365.com", 993, Tls),
        outgoing: ("smtp-mail.outlook.com", 587, StartTls),
        username_format: FullAddress,
        auth_kind: OAuth2RequiredNotice,
        strip_secret_spaces: false,
        notes: "微软已对个人版 Outlook/Hotmail 停用基础密码认证（需 OAuth2，当前版本暂不支持），个人账户大概率无法用密码登录；企业版或仍支持应用密码的账户可继续填入应用密码连接。",
        help_url: Some("https://support.microsoft.com/office/pop-imap-and-smtp-settings-for-outlook-com-d088b986-291d-42b8-9564-9c414e2aa040"),
        domains: &["outlook.com", "hotmail.com", "live.com", "msn.com"],
    },
    Preset {
        id: "fastmail",
        display_name: "Fastmail",
        incoming: ("imap.fastmail.com", 993, Tls),
        outgoing: ("smtp.fastmail.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AppPassword,
        strip_secret_spaces: true,
        notes: "在「Settings → Privacy & Security → Manage app passwords」新建权限含 IMAP/SMTP 的应用密码填入。",
        help_url: Some("https://www.fastmail.help/hc/en-us/articles/1500000278342"),
        domains: &["fastmail.com", "fastmail.fm"],
    },
    Preset {
        id: "yahoo",
        display_name: "Yahoo Mail",
        incoming: ("imap.mail.yahoo.com", 993, Tls),
        outgoing: ("smtp.mail.yahoo.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AppPassword,
        strip_secret_spaces: true,
        notes: "在「Account Info → Account Security」生成应用密码（App password）填入；不能使用网页登录密码。",
        help_url: Some("https://help.yahoo.com/kb/SLN4075.html"),
        domains: &["yahoo.com", "ymail.com", "rocketmail.com"],
    },
    Preset {
        id: "zoho",
        display_name: "Zoho Mail",
        incoming: ("imap.zoho.com", 993, Tls),
        outgoing: ("smtp.zoho.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AppPassword,
        strip_secret_spaces: true,
        notes: "先在 Zoho Mail「设置 → 邮件账户 → IMAP 访问」开启 IMAP；开启了双重认证的账户需生成应用专用密码填入。",
        help_url: Some("https://www.zoho.com/mail/help/imap-access.html"),
        domains: &["zoho.com", "zohomail.com"],
    },
    Preset {
        id: "zoho",
        display_name: "Zoho Mail（中国）",
        incoming: ("imap.zoho.com.cn", 993, Tls),
        outgoing: ("smtp.zoho.com.cn", 465, Tls),
        username_format: FullAddress,
        auth_kind: AppPassword,
        strip_secret_spaces: true,
        notes: "先在 Zoho Mail「设置 → 邮件账户 → IMAP 访问」开启 IMAP；开启了双重认证的账户需生成应用专用密码填入。",
        help_url: Some("https://www.zoho.com.cn/mail/help/imap-access.html"),
        domains: &["zohomail.cn", "zoho.com.cn"],
    },
    Preset {
        id: "proton_bridge",
        display_name: "Proton Mail（Bridge）",
        incoming: ("127.0.0.1", 1143, StartTls),
        outgoing: ("127.0.0.1", 1025, StartTls),
        username_format: FullAddress,
        auth_kind: LocalBridge,
        strip_secret_spaces: false,
        notes: "Proton Mail 需在本机运行 Proton Mail Bridge（付费账户可用），填入 Bridge 界面显示的用户名与 Bridge 专用密码（不是 Proton 登录密码）；Bridge 使用本地自签证书，需先在系统钥匙串中信任其证书。",
        help_url: Some("https://proton.me/support/protonmail-bridge-clients"),
        domains: &["proton.me", "protonmail.com", "protonmail.ch", "pm.me"],
    },
    Preset {
        id: "sina",
        display_name: "新浪邮箱",
        incoming: ("imap.sina.com", 993, Tls),
        outgoing: ("smtp.sina.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → 客户端 POP/IMAP/SMTP」开启 IMAP/SMTP 服务，按提示获取授权码填入。",
        help_url: Some("https://mail.sina.com.cn/"),
        domains: &["sina.com", "sina.cn"],
    },
    Preset {
        id: "sina",
        display_name: "新浪 VIP 邮箱",
        incoming: ("imap.vip.sina.com", 993, Tls),
        outgoing: ("smtp.vip.sina.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → 客户端 POP/IMAP/SMTP」开启 IMAP/SMTP 服务，按提示获取授权码填入。",
        help_url: Some("https://vip.sina.com.cn/"),
        domains: &["vip.sina.com"],
    },
    Preset {
        id: "sohu",
        display_name: "搜狐邮箱",
        incoming: ("imap.sohu.com", 993, Tls),
        outgoing: ("smtp.sohu.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → POP3/SMTP/IMAP」开启服务并设置客户端独立密码后填入。",
        help_url: Some("https://mail.sohu.com/"),
        domains: &["sohu.com"],
    },
    Preset {
        id: "china_mobile_139",
        display_name: "139 邮箱（中国移动）",
        incoming: ("imap.139.com", 993, Tls),
        outgoing: ("smtp.139.com", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → 账户与安全 → POP3/SMTP/IMAP」开启服务；如提示需要授权码，请按页面指引获取后填入。",
        help_url: Some("https://mail.10086.cn/"),
        domains: &["139.com"],
    },
    Preset {
        id: "china_telecom_189",
        display_name: "189 邮箱（中国电信）",
        incoming: ("imap.189.cn", 993, Tls),
        outgoing: ("smtp.189.cn", 465, Tls),
        username_format: FullAddress,
        auth_kind: AuthorizationCode,
        strip_secret_spaces: false,
        notes: "在网页版「设置 → POP3/SMTP/IMAP」开启服务；如提示需要授权码，请按页面指引获取后填入。",
        help_url: Some("https://webmail30.189.cn/"),
        domains: &["189.cn"],
    },
];

impl Preset {
    fn to_discovery(&self, username: &str) -> ProviderDiscovery {
        ProviderDiscovery {
            provider_id: self.id.to_string(),
            display_name: self.display_name.to_string(),
            source: "preset".to_string(),
            confidence: "verified".to_string(),
            incoming: endpoint(self.incoming),
            mailbox: DEFAULT_MAILBOX.to_string(),
            idle: true,
            outgoing: endpoint(self.outgoing),
            username_format: self.username_format,
            suggested_username: username.to_string(),
            auth_kind: self.auth_kind,
            strip_secret_spaces: self.strip_secret_spaces,
            notes: self.notes.to_string(),
            help_url: self.help_url.map(str::to_string),
        }
    }
}

fn endpoint((server, port, security): (&str, u16, SecurityMode)) -> DiscoveredEndpoint {
    DiscoveredEndpoint {
        server: server.to_string(),
        port,
        security,
    }
}

// —— 公开入口 ——

/// 常用主流邮箱预设卡片（每个服务商一张，用户名留空）。
pub fn list_presets() -> Vec<ProviderDiscovery> {
    let mut out: Vec<ProviderDiscovery> = Vec::new();
    for preset in PRESETS {
        if out.iter().any(|card| card.provider_id == preset.id) {
            continue; // 同一服务商的地区/VIP 变体只在按地址识别时出现
        }
        out.push(preset.to_discovery(""));
    }
    out
}

/// 纯本地识别：从邮箱地址或域名提取 domain 并匹配内置知识库（零网络）。
pub fn detect_from_address(input: &str) -> Option<ProviderDiscovery> {
    let parsed = parse_input(input)?;
    let preset = PRESETS
        .iter()
        .find(|preset| preset.domains.contains(&parsed.domain.as_str()))?;
    Some(preset.to_discovery(&parsed.username_for(preset.username_format)))
}

/// 识别服务商：本地知识库优先；未命中时校验域名（SSRF 防线），可选做受限的公网
/// TCP 可达性探测，否则返回启发式建议。
pub async fn discover_provider(
    input: &str,
    allow_network: bool,
) -> Result<ProviderDiscovery, String> {
    discover_provider_with_proxy(input, allow_network, None).await
}

/// 经可选代理连接器识别服务商。
///
/// 代理路径在建连前同样校验目标主机为合法公网非私有域名/非私有 IP 字面量（防 SSRF），
/// 远程 DNS 代理（`socks5h` / `http` / `https`）无需本地 DNS 解析即可正常工作，且允许
/// 代理服务器自身位于本机回环或局域网。
pub async fn discover_provider_with_proxy(
    input: &str,
    allow_network: bool,
    proxy: Option<&ProxyConnector>,
) -> Result<ProviderDiscovery, String> {
    if let Some(found) = detect_from_address(input) {
        return Ok(found);
    }
    let parsed = parse_input(input).ok_or_else(|| "请输入有效的邮箱地址或域名".to_string())?;
    let domain = validate_public_domain(&parsed.domain)?;
    let username = parsed.username_for(FullAddress);

    let mut discovery = heuristic(&domain, &username);
    if allow_network {
        let (incoming, outgoing) = probe_candidates(&domain, proxy).await;
        if incoming.is_some() || outgoing.is_some() {
            discovery.source = "tcp_probe".to_string();
            discovery.confidence = "high".to_string();
            if let Some(incoming) = incoming {
                discovery.incoming = incoming;
            }
            if let Some(outgoing) = outgoing {
                discovery.outgoing = outgoing;
            }
        }
    }
    Ok(discovery)
}

/// 校验并规范化一个**公网**域名：小写、去尾点、ASCII LDH 标签、至少两级、顶级标签
/// 非纯数字；拒绝 IP 字面量、`localhost` 与内网/保留后缀。
pub fn validate_public_domain(domain: &str) -> Result<String, String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() {
        return Err("域名不能为空".into());
    }
    let unbracketed = domain.trim_start_matches('[').trim_end_matches(']');
    if unbracketed.parse::<IpAddr>().is_ok() {
        return Err("不支持 IP 地址，请输入邮箱域名".into());
    }
    if !domain.is_ascii() {
        return Err("域名包含非 ASCII 字符，请输入 punycode 形式（xn--）".into());
    }
    if domain.len() > 253 {
        return Err("域名过长".into());
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return Err("不支持单标签内网主机名，请输入完整域名".into());
    }
    let valid_labels = labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    });
    if !valid_labels {
        return Err("域名格式无效".into());
    }
    let tld = labels[labels.len() - 1];
    if tld.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("不支持 IP 地址，请输入邮箱域名".into());
    }
    const BLOCKED_SUFFIXES: &[&str] = &[
        "localhost",
        "local",
        "internal",
        "intranet",
        "lan",
        "home",
        "corp",
        "localdomain",
        "home.arpa",
        "arpa",
        "invalid",
        "test",
        "example",
        "onion",
    ];
    if BLOCKED_SUFFIXES
        .iter()
        .any(|suffix| domain == *suffix || domain.ends_with(&format!(".{suffix}")))
    {
        return Err("不支持本机、内网或保留域名".into());
    }
    Ok(domain)
}

/// 是否为可主动连接的公网单播地址。拒绝 loopback / RFC1918 私网 / link-local /
/// ULA / unspecified / CGNAT / 组播 / 广播 / 文档与基准测试保留段（含 IPv4 映射）。
pub fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => is_public_ipv6(v6),
    }
}

fn is_public_ipv4(ip: &Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // CGNAT 100.64/10
        || (a == 192 && b == 0 && c == 0) // IETF 协议分配 192.0.0/24
        || (a == 198 && (18..20).contains(&b)) // 基准测试 198.18/15
        || a >= 240) // 保留 240/4
}

fn is_public_ipv6(ip: &Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(&v4);
    }
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00 // ULA fc00::/7
        || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (first & 0xffc0) == 0xfec0 // 已废弃 site-local fec0::/10
        || (first == 0x2001 && ip.segments()[1] == 0x0db8) // 文档 2001:db8::/32
        || first == 0x0064 // NAT64 64:ff9b::/96 等内部翻译段
        || first == 0x0000) // 其余 ::/16（含 IPv4-compatible）
}

// —— 内部实现 ——

struct ParsedInput {
    local: Option<String>,
    domain: String,
}

impl ParsedInput {
    fn username_for(&self, format: UsernameFormat) -> String {
        match (&self.local, format) {
            (None, _) => String::new(),
            (Some(local), UsernameFormat::LocalPart) => local.clone(),
            (Some(local), UsernameFormat::FullAddress) => format!("{local}@{}", self.domain),
        }
    }
}

/// 解析 `alice@example.com` / `example.com` / `@example.com`。
fn parse_input(input: &str) -> Option<ParsedInput> {
    let input = input.trim();
    if input.is_empty() || input.len() > 320 || input.chars().any(char::is_whitespace) {
        return None;
    }
    let (local, domain) = match input.rsplit_once('@') {
        Some((local, domain)) => {
            let local = (!local.is_empty()).then(|| local.to_string());
            (local, domain)
        }
        None => (None, input),
    };
    if local.as_deref().is_some_and(|local| {
        local.len() > 64 || local.contains('@') || local.chars().any(char::is_control)
    }) {
        return None;
    }
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() {
        return None;
    }
    Some(ParsedInput { local, domain })
}

fn heuristic(domain: &str, username: &str) -> ProviderDiscovery {
    ProviderDiscovery {
        provider_id: "custom".to_string(),
        display_name: domain.to_string(),
        source: "heuristic".to_string(),
        confidence: "guessed".to_string(),
        incoming: DiscoveredEndpoint {
            server: format!("imap.{domain}"),
            port: 993,
            security: SecurityMode::Implicit,
        },
        mailbox: DEFAULT_MAILBOX.to_string(),
        idle: true,
        outgoing: DiscoveredEndpoint {
            server: format!("smtp.{domain}"),
            port: 465,
            security: SecurityMode::Implicit,
        },
        username_format: UsernameFormat::FullAddress,
        suggested_username: username.to_string(),
        auth_kind: AuthGuideKind::AccountPassword,
        strip_secret_spaces: false,
        notes: "未识别的邮箱服务商，以下为推测配置：请在服务商帮助页核对 IMAP/SMTP 服务器、端口与加密方式（993/465 通常使用 TLS，143/587 通常使用 STARTTLS），并确认是否需要授权码或应用专用密码。".to_string(),
        help_url: None,
    }
}

/// 收件候选按优先级排列，发件同理。
fn candidates(domain: &str) -> (Vec<DiscoveredEndpoint>, Vec<DiscoveredEndpoint>) {
    let endpoint = |prefix: &str, port: u16, security: SecurityMode| DiscoveredEndpoint {
        server: format!("{prefix}.{domain}"),
        port,
        security,
    };
    (
        vec![
            endpoint("imap", 993, SecurityMode::Implicit),
            endpoint("mail", 993, SecurityMode::Implicit),
        ],
        vec![
            endpoint("smtp", 465, SecurityMode::Implicit),
            endpoint("smtp", 587, SecurityMode::StartTls),
        ],
    )
}

/// 在总预算内并发探测候选端点；返回各方向优先级最高的可达端点。
async fn probe_candidates(
    domain: &str,
    proxy: Option<&ProxyConnector>,
) -> (Option<DiscoveredEndpoint>, Option<DiscoveredEndpoint>) {
    let (incoming, outgoing) = candidates(domain);
    let incoming_count = incoming.len();
    let all: Vec<DiscoveredEndpoint> = incoming.into_iter().chain(outgoing).collect();
    let mut reachable = vec![false; all.len()];

    let deadline = tokio::time::Instant::now() + NETWORK_BUDGET;
    let mut tasks = tokio::task::JoinSet::new();
    for (index, candidate) in all.iter().enumerate() {
        let server = candidate.server.clone();
        let port = candidate.port;
        let scheme = match (index < incoming_count, candidate.security) {
            (true, SecurityMode::Implicit) => TargetScheme::Imaps,
            (true, SecurityMode::StartTls) => TargetScheme::Imap,
            (false, SecurityMode::Implicit) => TargetScheme::Smtps,
            (false, SecurityMode::StartTls) => TargetScheme::Smtp,
        };
        let proxy = proxy.cloned();
        tasks.spawn(async move {
            (
                index,
                tcp_reachable_public(&server, port, scheme, proxy.as_ref()).await,
            )
        });
    }
    while let Ok(Some(joined)) = tokio::time::timeout_at(deadline, tasks.join_next()).await {
        if let Ok((index, true)) = joined {
            reachable[index] = true;
        }
    }
    tasks.abort_all();

    let pick = |range: std::ops::Range<usize>| {
        range
            .into_iter()
            .find(|index| reachable[*index])
            .map(|index| all[index].clone())
    };
    (pick(0..incoming_count), pick(incoming_count..all.len()))
}

/// 校验目标为公网主机后再探测 TCP 可达性：
/// - 若经代理：先校验目标主机不是私有/保留域名或 IP 字面量（允许代理自身位于回环/局域网）。
///   对本地 DNS 的 `socks5`，仍先在本地解析并校验全部解析 IP 均为公网地址；
///   对远程 DNS 的 `socks5h` / `http` / `https`，直接交由代理建立隧道。
/// - 若直连：本地解析 `server:port`，任一解析地址为非公网即放弃，仅连接已校验地址。
async fn tcp_reachable_public(
    server: &str,
    port: u16,
    scheme: TargetScheme,
    proxy: Option<&ProxyConnector>,
) -> bool {
    if validate_public_domain(server).is_err() {
        return false;
    }
    if let Some(connector) = proxy {
        let target = ProxyTarget::new(server, port, scheme);
        if let ResolvedRoute::Proxy(endpoint) = connector.resolve(&target) {
            if endpoint.protocol == ProxyProtocol::Socks5 {
                let Ok(resolved) = tokio::net::lookup_host((server, port)).await else {
                    return false;
                };
                let addresses: Vec<SocketAddr> = resolved.collect();
                if addresses.is_empty()
                    || addresses.iter().any(|address| !is_public_ip(&address.ip()))
                {
                    return false;
                }
            }
            let opts = ConnectOptions {
                connect_per_addr: NETWORK_BUDGET,
                connect_total: NETWORK_BUDGET,
                proxy_handshake: NETWORK_BUDGET,
                tcp_nodelay: true,
                tcp_keepalive: false,
            };
            if let Ok((stream, _route)) = connector.connect(&target, opts).await {
                drop(stream);
                return true;
            }
            return false;
        }
    }
    let Ok(resolved) = tokio::net::lookup_host((server, port)).await else {
        return false;
    };
    let addresses: Vec<SocketAddr> = resolved.collect();
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(&address.ip())) {
        return false;
    }
    for address in addresses {
        if let Ok(stream) = TcpStream::connect(address).await {
            drop(stream);
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ip_filter_rejects_internal_ranges() {
        for blocked in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.0.1",
            "2001:db8::1",
        ] {
            let ip: IpAddr = blocked.parse().unwrap();
            assert!(!is_public_ip(&ip), "{blocked} 必须被拒绝");
        }
        for allowed in [
            "8.8.8.8",
            "1.1.1.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            let ip: IpAddr = allowed.parse().unwrap();
            assert!(is_public_ip(&ip), "{allowed} 应视为公网");
        }
    }

    #[test]
    fn parse_input_handles_addresses_and_bare_domains() {
        let parsed = parse_input("  Alice@Example.COM. ").unwrap();
        assert_eq!(parsed.local.as_deref(), Some("Alice"));
        assert_eq!(parsed.domain, "example.com");
        assert_eq!(parsed.username_for(UsernameFormat::LocalPart), "Alice");
        let bare = parse_input("@example.com").unwrap();
        assert!(bare.local.is_none());
        assert!(parse_input("a b@example.com").is_none());
        assert!(parse_input("alice@").is_none());
    }

    #[test]
    fn candidate_order_prefers_imap_and_implicit_submission() {
        let (incoming, outgoing) = candidates("example.org");
        assert_eq!(incoming[0].server, "imap.example.org");
        assert_eq!(incoming[1].server, "mail.example.org");
        assert_eq!(
            (outgoing[0].port, outgoing[0].security),
            (465, SecurityMode::Implicit)
        );
        assert_eq!(
            (outgoing[1].port, outgoing[1].security),
            (587, SecurityMode::StartTls)
        );
    }

    #[test]
    fn presets_are_unique_per_domain_and_use_https_help() {
        let mut seen = std::collections::BTreeSet::new();
        for preset in PRESETS {
            for domain in preset.domains {
                assert!(seen.insert(*domain), "域名 {domain} 重复登记");
            }
            if let Some(url) = preset.help_url {
                assert!(url.starts_with("https://"), "{url}");
            }
        }
    }
}
