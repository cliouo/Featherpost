//! Proxy configuration, route, target, and error data model.
//!
//! Security invariant: passwords are NEVER printed in `Debug`, `Display`, error messages,
//! or redacted URLs.

use std::fmt;
use std::net::{IpAddr, Ipv6Addr};

use serde::{Deserialize, Serialize};

use crate::bypass::BypassList;

pub const MAX_HOST_BYTES: usize = 255;
pub const MAX_USERNAME_BYTES: usize = 255;
pub const MAX_PASSWORD_BYTES: usize = 1024;
pub const MAX_BYPASS_ENTRIES: usize = 256;
pub const MAX_BYPASS_ENTRY_BYTES: usize = 255;

/// Proxy operating mode. Default is [`ProxyMode::System`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    #[default]
    System,
    Direct,
    Custom,
}

impl ProxyMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Direct => "direct",
            Self::Custom => "custom",
        }
    }
}

/// Supported proxy tunnel protocols.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyProtocol {
    Http,
    Https,
    Socks5,
    Socks5h,
}

impl ProxyProtocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks5 => "socks5",
            Self::Socks5h => "socks5h",
        }
    }

    pub const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
            Self::Socks5 | Self::Socks5h => 1080,
        }
    }

    pub fn parse_scheme(scheme: &str) -> Result<Self, String> {
        match scheme.trim().to_ascii_lowercase().as_str() {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            "socks5" => Ok(Self::Socks5),
            "socks5h" => Ok(Self::Socks5h),
            other => Err(format!(
                "不支持的代理协议 `{other}`（支持 http / https / socks5 / socks5h）"
            )),
        }
    }
}

/// User-configured custom proxy settings.
///
/// Preserved across switches to `system` / `direct` mode.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomProxyConfig {
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default)]
    pub bypass: Vec<String>,
}

impl fmt::Debug for CustomProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomProxyConfig")
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("bypass", &self.bypass)
            .finish()
    }
}

impl CustomProxyConfig {
    pub fn has_password(&self) -> bool {
        self.password.as_deref().is_some_and(|p| !p.is_empty())
    }

    pub fn without_secret(&self) -> Self {
        Self {
            protocol: self.protocol,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password: None,
            bypass: self.bypass.clone(),
        }
    }

    pub fn to_endpoint(&self) -> ProxyEndpoint {
        ProxyEndpoint {
            protocol: self.protocol,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password: self.password.clone(),
        }
    }
}

/// Top-level proxy preference configuration.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    #[serde(default)]
    pub mode: ProxyMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<CustomProxyConfig>,
}

impl fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("mode", &self.mode)
            .field("custom", &self.custom)
            .finish()
    }
}

impl ProxyConfig {
    pub fn has_password(&self) -> bool {
        self.custom.as_ref().is_some_and(CustomProxyConfig::has_password)
    }

    pub fn without_secret(&self) -> Self {
        Self {
            mode: self.mode,
            custom: self.custom.as_ref().map(CustomProxyConfig::without_secret),
        }
    }
}

/// A resolved proxy server endpoint.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyEndpoint {
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl fmt::Debug for ProxyEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyEndpoint")
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl ProxyEndpoint {
    pub fn without_secret(&self) -> Self {
        Self {
            protocol: self.protocol,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password: None,
        }
    }

    pub fn with_protocol(mut self, protocol: ProxyProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// Safe human-readable representation that NEVER includes any password.
    pub fn redacted_url(&self) -> String {
        let authority = format_host_port(&self.host, self.port);
        match self.username.as_deref().filter(|u| !u.is_empty()) {
            Some(user) => format!("{}://{}@{authority}", self.protocol.as_str(), user),
            None => format!("{}://{authority}", self.protocol.as_str()),
        }
    }
}

/// Snapshot of OS / environment proxy settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemProxySnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<ProxyEndpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub https: Option<ProxyEndpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socks5: Option<ProxyEndpoint>,
    #[serde(default)]
    pub bypass: Vec<String>,
    #[serde(default)]
    pub exclude_simple_hostnames: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pac_url: Option<String>,
    #[serde(default)]
    pub source: String,
}

impl SystemProxySnapshot {
    pub fn has_any_proxy(&self) -> bool {
        self.http.is_some() || self.https.is_some() || self.socks5.is_some()
    }

    pub fn without_secrets(&self) -> Self {
        Self {
            http: self.http.as_ref().map(ProxyEndpoint::without_secret),
            https: self.https.as_ref().map(ProxyEndpoint::without_secret),
            socks5: self.socks5.as_ref().map(ProxyEndpoint::without_secret),
            bypass: self.bypass.clone(),
            exclude_simple_hostnames: self.exclude_simple_hostnames,
            pac_url: self.pac_url.clone(),
            source: self.source.clone(),
        }
    }
}

/// Protocol scheme of the target connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetScheme {
    Imap,
    Imaps,
    Smtp,
    Smtps,
    Http,
    Https,
    RawTcp,
}

impl TargetScheme {
    pub const fn infer_from_port(port: u16) -> Self {
        match port {
            993 => Self::Imaps,
            143 => Self::Imap,
            465 => Self::Smtps,
            587 | 25 => Self::Smtp,
            443 => Self::Https,
            80 => Self::Http,
            _ => Self::RawTcp,
        }
    }
}

/// Destination host, port, and scheme to connect to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyTarget {
    pub host: String,
    pub port: u16,
    pub scheme: TargetScheme,
}

impl ProxyTarget {
    pub fn new(host: impl Into<String>, port: u16, scheme: TargetScheme) -> Self {
        Self {
            host: host.into(),
            port,
            scheme,
        }
    }

    /// Parse `host:port` or `[ipv6]:port` into a validated [`ProxyTarget`].
    pub fn parse_host_port(raw: &str, scheme: Option<TargetScheme>) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("目标地址不能为空".into());
        }
        let (host_raw, port_str) = if let Some(rest) = trimmed.strip_prefix('[') {
            let (ipv6, tail) = rest
                .split_once(']')
                .ok_or_else(|| "IPv6 目标地址缺少右括号 `]`".to_string())?;
            let port_str = tail
                .strip_prefix(':')
                .ok_or_else(|| "目标地址必须包含 `:port`".to_string())?;
            if ipv6.parse::<Ipv6Addr>().is_err() {
                return Err(format!("无效的 IPv6 地址 `{ipv6}`"));
            }
            (ipv6, port_str)
        } else {
            let (host, port_str) = trimmed
                .rsplit_once(':')
                .ok_or_else(|| "目标地址格式应为 `host:port`".to_string())?;
            if host.contains(':') {
                return Err("IPv6 目标地址必须使用 `[addr]:port` 格式".into());
            }
            (host, port_str)
        };
        let host = normalize_host(host_raw)?;
        let port: u16 = port_str
            .parse()
            .map_err(|_| format!("目标端口 `{port_str}` 无效"))?;
        if port == 0 {
            return Err("目标端口必须在 1..=65535 之间".into());
        }
        Ok(Self {
            host,
            port,
            scheme: scheme.unwrap_or_else(|| TargetScheme::infer_from_port(port)),
        })
    }
}

/// Output of per-connection route resolution.
#[derive(Clone, PartialEq, Eq)]
pub enum ResolvedRoute {
    Direct,
    Proxy(ProxyEndpoint),
}

impl fmt::Debug for ResolvedRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct => f.write_str("ResolvedRoute::Direct"),
            Self::Proxy(ep) => f.debug_tuple("ResolvedRoute::Proxy").field(ep).finish(),
        }
    }
}

impl ResolvedRoute {
    /// Safe summary for API responses and logs (never contains passwords).
    pub fn describe(&self) -> String {
        match self {
            Self::Direct => "direct".to_string(),
            Self::Proxy(ep) => ep.redacted_url(),
        }
    }
}

/// Error classification for proxy and direct connection attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyErrorKind {
    InvalidConfig,
    DirectConnectFailed,
    DirectConnectTimeout,
    ProxyConnectFailed,
    ProxyTimeout,
    AuthRequired,
    TunnelFailed,
}

/// Error produced during proxy routing or tunnel establishment.
///
/// Guaranteed never to echo proxy passwords.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyError {
    pub kind: ProxyErrorKind,
    pub message: String,
    pub detail: Option<String>,
    pub status_code: Option<u16>,
    pub socks_rep: Option<u8>,
}

impl ProxyError {
    pub fn invalid_config(message: impl Into<String>) -> Self {
        Self {
            kind: ProxyErrorKind::InvalidConfig,
            message: message.into(),
            detail: None,
            status_code: None,
            socks_rep: None,
        }
    }

    pub fn direct_failed(message: impl Into<String>, detail: Option<String>) -> Self {
        Self {
            kind: ProxyErrorKind::DirectConnectFailed,
            message: message.into(),
            detail,
            status_code: None,
            socks_rep: None,
        }
    }

    pub fn direct_timeout(message: impl Into<String>) -> Self {
        Self {
            kind: ProxyErrorKind::DirectConnectTimeout,
            message: message.into(),
            detail: None,
            status_code: None,
            socks_rep: None,
        }
    }

    pub fn proxy_connect_failed(message: impl Into<String>, detail: Option<String>) -> Self {
        Self {
            kind: ProxyErrorKind::ProxyConnectFailed,
            message: message.into(),
            detail,
            status_code: None,
            socks_rep: None,
        }
    }

    pub fn proxy_timeout(message: impl Into<String>) -> Self {
        Self {
            kind: ProxyErrorKind::ProxyTimeout,
            message: message.into(),
            detail: None,
            status_code: None,
            socks_rep: None,
        }
    }

    pub fn auth_required(status_code: Option<u16>, message: impl Into<String>) -> Self {
        Self {
            kind: ProxyErrorKind::AuthRequired,
            message: message.into(),
            detail: None,
            status_code,
            socks_rep: None,
        }
    }

    pub fn tunnel_failed(
        status_code: Option<u16>,
        socks_rep: Option<u8>,
        message: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            kind: ProxyErrorKind::TunnelFailed,
            message: message.into(),
            detail,
            status_code,
            socks_rep,
        }
    }

    pub fn public_message(&self) -> String {
        match &self.detail {
            Some(detail) if !detail.is_empty() => format!("{}: {}", self.message, detail),
            _ => self.message.clone(),
        }
    }
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.public_message())
    }
}

impl std::error::Error for ProxyError {}

/// Format `host:port`, wrapping IPv6 literals in square brackets.
pub fn format_host_port(host: &str, port: u16) -> String {
    let stripped = host.trim_start_matches('[').trim_end_matches(']');
    if stripped.contains(':') {
        format!("[{stripped}]:{port}")
    } else {
        format!("{stripped}:{port}")
    }
}

/// Normalize and validate a proxy or target hostname/IP.
pub fn normalize_host(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("主机名不能为空".into());
    }
    let unbracketed = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let inner = &trimmed[1..trimmed.len() - 1];
        if inner.parse::<Ipv6Addr>().is_err() {
            return Err(format!("无效的 IPv6 主机 `{trimmed}`"));
        }
        inner
    } else {
        trimmed
    };
    if unbracketed.is_empty() || unbracketed.len() > MAX_HOST_BYTES {
        return Err(format!("主机名长度必须在 1..={MAX_HOST_BYTES} 字节之间"));
    }
    if unbracketed.chars().any(|c| {
        c.is_control()
            || c.is_whitespace()
            || matches!(c, '/' | '@' | '?' | '#' | '\\')
    }) {
        return Err("主机名包含非法字符或控制字符".into());
    }
    // Reject non-IPv6 colons (e.g. accidental host:port in host field).
    if unbracketed.contains(':') && unbracketed.parse::<Ipv6Addr>().is_err() {
        return Err("主机名不能包含端口或非法冒号".into());
    }
    Ok(unbracketed.to_string())
}

/// Validate and normalize a [`CustomProxyConfig`].
pub fn validate_custom_config(cfg: &CustomProxyConfig) -> Result<CustomProxyConfig, String> {
    let host = normalize_host(&cfg.host)?;
    if cfg.port == 0 {
        return Err("代理端口必须在 1..=65535 之间".into());
    }
    let username = match cfg.username.as_deref() {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                if trimmed.len() > MAX_USERNAME_BYTES {
                    return Err(format!("代理用户名过长（上限 {MAX_USERNAME_BYTES} 字节）"));
                }
                if trimmed.chars().any(char::is_control) || trimmed.contains(':') {
                    return Err("代理用户名不能包含控制字符或冒号".into());
                }
                Some(trimmed.to_string())
            }
        }
    };
    let password = match cfg.password.as_deref() {
        None | Some("") => None,
        Some(raw) => {
            if raw.len() > MAX_PASSWORD_BYTES {
                return Err(format!("代理密码过长（上限 {MAX_PASSWORD_BYTES} 字节）"));
            }
            if raw.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
                return Err("代理密码不能包含换行符或 NUL 字符".into());
            }
            Some(raw.to_string())
        }
    };
    if cfg.bypass.len() > MAX_BYPASS_ENTRIES {
        return Err(format!("旁路规则超过 {MAX_BYPASS_ENTRIES} 条上限"));
    }
    let mut bypass = Vec::with_capacity(cfg.bypass.len());
    for entry in &cfg.bypass {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.len() > MAX_BYPASS_ENTRY_BYTES {
            return Err(format!("旁路条目过长（上限 {MAX_BYPASS_ENTRY_BYTES} 字节）"));
        }
        if trimmed.chars().any(char::is_control) {
            return Err("旁路条目不能包含控制字符".into());
        }
        bypass.push(trimmed.to_string());
    }
    BypassList::parse_strict(&bypass)?;

    Ok(CustomProxyConfig {
        protocol: cfg.protocol,
        host,
        port: cfg.port,
        username,
        password,
        bypass,
    })
}

/// Validate and normalize a standalone [`ProxyEndpoint`].
pub fn validate_endpoint(endpoint: &ProxyEndpoint) -> Result<ProxyEndpoint, String> {
    let validated = validate_custom_config(&CustomProxyConfig {
        protocol: endpoint.protocol,
        host: endpoint.host.clone(),
        port: endpoint.port,
        username: endpoint.username.clone(),
        password: endpoint.password.clone(),
        bypass: Vec::new(),
    })?;
    Ok(validated.to_endpoint())
}

/// Validate and normalize a full [`ProxyConfig`].
pub fn validate_proxy_config(cfg: &ProxyConfig) -> Result<ProxyConfig, String> {
    let custom = cfg
        .custom
        .as_ref()
        .map(validate_custom_config)
        .transpose()?;
    if cfg.mode == ProxyMode::Custom && custom.is_none() {
        return Err("mode = \"custom\" 时必须提供 custom 代理配置".into());
    }
    Ok(ProxyConfig {
        mode: cfg.mode,
        custom,
    })
}

/// Parse a proxy URL such as `socks5h://user:pass@127.0.0.1:7890` or `http://proxy.local:8080`.
pub fn parse_proxy_url(raw: &str) -> Result<ProxyEndpoint, String> {
    parse_proxy_url_inner(raw, None)
}

/// Parse a proxy URL, applying `default_protocol` if no `scheme://` prefix is present
/// (useful for `HTTP_PROXY=127.0.0.1:7890` or Windows `ProxyServer`).
pub fn parse_proxy_url_with_default_scheme(
    raw: &str,
    default_protocol: ProxyProtocol,
) -> Result<ProxyEndpoint, String> {
    parse_proxy_url_inner(raw, Some(default_protocol))
}

fn parse_proxy_url_inner(
    raw: &str,
    default_protocol: Option<ProxyProtocol>,
) -> Result<ProxyEndpoint, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("代理 URL 不能为空".into());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("代理 URL 不能包含控制字符".into());
    }
    let (protocol, after_scheme) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (ProxyProtocol::parse_scheme(scheme)?, rest),
        None => {
            let proto = default_protocol.ok_or_else(|| {
                "代理 URL 缺少协议前缀（例如 socks5h://、http://、https://、socks5://）"
                    .to_string()
            })?;
            (proto, trimmed)
        }
    };
    // Strip optional trailing slash/path (e.g. `http://127.0.0.1:7890/`).
    let authority = match after_scheme.split_once('/') {
        Some((auth, path)) => {
            if !path.is_empty() {
                return Err("代理 URL 不支持路径或查询参数".into());
            }
            auth
        }
        None => after_scheme,
    };
    if authority.contains('?') || authority.contains('#') {
        return Err("代理 URL 不支持查询参数或片段".into());
    }
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, hp)) => (Some(u), hp),
        None => (None, authority),
    };
    let (username, password) = match userinfo {
        None => (None, None),
        Some(ui) => match ui.split_once(':') {
            Some((u, p)) => (
                non_empty_decoded(u)?,
                non_empty_decoded(p)?,
            ),
            None => (non_empty_decoded(ui)?, None),
        },
    };
    let (host_raw, port) = split_host_and_optional_port(hostport, protocol.default_port())?;
    let host = normalize_host(host_raw)?;
    if port == 0 {
        return Err("代理端口必须在 1..=65535 之间".into());
    }
    // Reuse CustomProxyConfig validation for username/password checks.
    let validated = validate_custom_config(&CustomProxyConfig {
        protocol,
        host,
        port,
        username,
        password,
        bypass: Vec::new(),
    })?;
    Ok(validated.to_endpoint())
}

fn split_host_and_optional_port(hostport: &str, default_port: u16) -> Result<(&str, u16), String> {
    if hostport.is_empty() {
        return Err("代理地址缺少主机名".into());
    }
    if let Some(rest) = hostport.strip_prefix('[') {
        let (ipv6, tail) = rest
            .split_once(']')
            .ok_or_else(|| "IPv6 代理地址缺少右括号 `]`".to_string())?;
        if ipv6.parse::<Ipv6Addr>().is_err() {
            return Err(format!("无效的 IPv6 地址 `{ipv6}`"));
        }
        if tail.is_empty() {
            return Ok((ipv6, default_port));
        }
        let port_str = tail
            .strip_prefix(':')
            .ok_or_else(|| "代理地址端口格式无效".to_string())?;
        let port: u16 = port_str
            .parse()
            .map_err(|_| format!("代理端口 `{port_str}` 无效"))?;
        return Ok((ipv6, port));
    }
    if hostport.parse::<IpAddr>().is_ok() {
        return Ok((hostport, default_port));
    }
    match hostport.rsplit_once(':') {
        Some((host, port_str)) if !host.contains(':') => {
            let port: u16 = port_str
                .parse()
                .map_err(|_| format!("代理端口 `{port_str}` 无效"))?;
            Ok((host, port))
        }
        Some(_) => Err("IPv6 代理地址必须使用 `[addr]:port` 格式".into()),
        None => Ok((hostport, default_port)),
    }
}

/// Minimal RFC 3986 percent-decoding for proxy URL userinfo (no external dependency needed).
fn non_empty_decoded(raw: &str) -> Result<Option<String>, String> {
    if raw.is_empty() {
        return Ok(None);
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("代理 URL 认证字段包含不完整的百分号编码".into());
            }
            let hi = hex_val(bytes[i + 1])?;
            let lo = hex_val(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let text = String::from_utf8(out).map_err(|_| "代理 URL 认证字段不是有效的 UTF-8".to_string())?;
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text))
    }
}

fn hex_val(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("代理 URL 认证字段包含无效的百分号编码".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_password_in_all_types() {
        let custom = CustomProxyConfig {
            protocol: ProxyProtocol::Socks5h,
            host: "127.0.0.1".into(),
            port: 7890,
            username: Some("alice".into()),
            password: Some("super-secret-password".into()),
            bypass: vec![],
        };
        let cfg = ProxyConfig {
            mode: ProxyMode::Custom,
            custom: Some(custom.clone()),
        };
        let ep = custom.to_endpoint();
        let route = ResolvedRoute::Proxy(ep.clone());

        for rendered in [
            format!("{custom:?}"),
            format!("{cfg:?}"),
            format!("{ep:?}"),
            format!("{route:?}"),
            ep.redacted_url(),
            route.describe(),
        ] {
            assert!(
                !rendered.contains("super-secret-password"),
                "leaked password in: {rendered}"
            );
        }
        assert!(format!("{custom:?}").contains("[REDACTED]"));
        assert_eq!(ep.redacted_url(), "socks5h://alice@127.0.0.1:7890");
    }

    #[test]
    fn parse_proxy_url_handles_schemes_auth_and_ipv6() {
        let ep = parse_proxy_url("socks5h://alice:p%40ss%3A1@127.0.0.1:7890").unwrap();
        assert_eq!(ep.protocol, ProxyProtocol::Socks5h);
        assert_eq!(ep.host, "127.0.0.1");
        assert_eq!(ep.port, 7890);
        assert_eq!(ep.username.as_deref(), Some("alice"));
        assert_eq!(ep.password.as_deref(), Some("p@ss:1"));

        let v6 = parse_proxy_url("https://[2001:db8::1]:8443/").unwrap();
        assert_eq!(v6.protocol, ProxyProtocol::Https);
        assert_eq!(v6.host, "2001:db8::1");
        assert_eq!(v6.port, 8443);

        let default_port = parse_proxy_url("socks5://proxy.example.com").unwrap();
        assert_eq!(default_port.port, 1080);

        let env_bare =
            parse_proxy_url_with_default_scheme("127.0.0.1:8080", ProxyProtocol::Http).unwrap();
        assert_eq!(env_bare.protocol, ProxyProtocol::Http);
        assert_eq!(env_bare.port, 8080);

        assert!(parse_proxy_url("ftp://127.0.0.1:21").is_err());
        assert!(parse_proxy_url("http://127.0.0.1:0").is_err());
        assert!(parse_proxy_url("http://evil.com\r\nX-Inject: 1:8080").is_err());
    }

    #[test]
    fn target_host_port_parser() {
        let t = ProxyTarget::parse_host_port("imap.gmail.com:993", None).unwrap();
        assert_eq!(t.host, "imap.gmail.com");
        assert_eq!(t.port, 993);
        assert_eq!(t.scheme, TargetScheme::Imaps);

        let v6 = ProxyTarget::parse_host_port("[::1]:143", None).unwrap();
        assert_eq!(v6.host, "::1");
        assert_eq!(v6.port, 143);
        assert_eq!(v6.scheme, TargetScheme::Imap);

        assert!(ProxyTarget::parse_host_port("imap.gmail.com", None).is_err());
        assert!(ProxyTarget::parse_host_port("imap.gmail.com:0", None).is_err());
        assert!(ProxyTarget::parse_host_port("::1:993", None).is_err());
    }
}
