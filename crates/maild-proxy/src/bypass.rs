//! Proxy bypass list parser and matcher.
//!
//! Supported syntax:
//! - Exact host (`example.com`, `mail.internal`)
//! - Domain suffix / wildcard (`.example.com`, `*.example.com`)
//! - Port-scoped host/IP/suffix (`host:port`, `[::1]:8080`, `.example.com:993`, `*.example.com:443`)
//! - IPv4 / IPv6 literals
//! - CIDR networks via `ipnet` (including macOS shorthand such as `169.254/16`, `10/8`)
//! - `<local>` (simple hostnames without dots)
//! - `*` (match all)

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnet::{IpNet, Ipv4Net};

/// Mandatory loopback check: `localhost`, `*.localhost`, `127.0.0.0/8`, `::1`, and
/// IPv4-mapped `::ffff:127.0.0.0/104`.
pub fn is_loopback_host(raw_host: &str) -> bool {
    let host = normalize_target_host(raw_host);
    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => v4.is_loopback(),
            IpAddr::V6(v6) => {
                v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
            }
        };
    }
    false
}

/// A parsed bypass rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BypassRule {
    Any,
    Local,
    Cidr(IpNet),
    Ip { ip: IpAddr, port: Option<u16> },
    /// `.example.com`: matches `example.com` and any subdomain `*.example.com`.
    DotSuffix { suffix: String, port: Option<u16> },
    /// `*.example.com`: matches subdomains `*.example.com` (not the bare apex).
    WildcardSubdomain { suffix: String, port: Option<u16> },
    /// Exact hostname match.
    ExactHost { host: String, port: Option<u16> },
}

/// Compiled bypass list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BypassList {
    rules: Vec<BypassRule>,
    exclude_simple_hostnames: bool,
}

impl BypassList {
    /// Parse a slice of rule strings, silently skipping empty or malformed OS/env entries.
    pub fn parse_entries(entries: &[String]) -> Self {
        let mut rules = Vec::with_capacity(entries.len());
        for raw in entries {
            for part in raw.split([',', ';']) {
                let trimmed = part.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(rule) = parse_rule(trimmed) {
                    rules.push(rule);
                }
            }
        }
        Self {
            rules,
            exclude_simple_hostnames: false,
        }
    }

    /// Parse a slice of rule strings strictly (used for validating user custom proxy config).
    pub fn parse_strict(entries: &[String]) -> Result<Self, String> {
        let mut rules = Vec::with_capacity(entries.len());
        for raw in entries {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            rules.push(parse_rule(trimmed)?);
        }
        Ok(Self {
            rules,
            exclude_simple_hostnames: false,
        })
    }

    /// Parse a comma- or semicolon-delimited string (`NO_PROXY` / Windows `ProxyOverride`).
    pub fn from_delimited(raw: &str) -> Self {
        let entries: Vec<String> = raw
            .split([',', ';'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        Self::parse_entries(&entries)
    }

    pub fn with_exclude_simple_hostnames(mut self, exclude: bool) -> Self {
        self.exclude_simple_hostnames = exclude;
        self
    }

    pub fn rules(&self) -> &[BypassRule] {
        &self.rules
    }

    pub fn matches(&self, raw_host: &str, port: u16) -> bool {
        let host = normalize_target_host(raw_host);
        if host.is_empty() {
            return false;
        }
        if self.exclude_simple_hostnames && is_simple_hostname(&host) {
            return true;
        }
        let target_ip = host.parse::<IpAddr>().ok();
        for rule in &self.rules {
            if rule_matches(rule, &host, target_ip, port) {
                return true;
            }
        }
        false
    }
}

fn is_simple_hostname(normalized_host: &str) -> bool {
    !normalized_host.contains('.') && !normalized_host.contains(':')
}

fn normalize_target_host(raw: &str) -> String {
    let trimmed = raw.trim();
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    unbracketed.trim_end_matches('.').to_ascii_lowercase()
}

fn rule_matches(rule: &BypassRule, host: &str, target_ip: Option<IpAddr>, port: u16) -> bool {
    match rule {
        BypassRule::Any => true,
        BypassRule::Local => is_simple_hostname(host),
        BypassRule::Cidr(net) => {
            let Some(ip) = target_ip else {
                return false;
            };
            if net.contains(&ip) {
                return true;
            }
            if let IpAddr::V6(v6) = ip {
                if let Some(v4) = v6.to_ipv4_mapped() {
                    return net.contains(&IpAddr::V4(v4));
                }
            }
            false
        }
        BypassRule::Ip {
            ip: rule_ip,
            port: rule_port,
        } => {
            if rule_port.is_some_and(|p| p != port) {
                return false;
            }
            let Some(ip) = target_ip else {
                return false;
            };
            if ip == *rule_ip {
                return true;
            }
            if let (IpAddr::V6(v6), IpAddr::V4(rule_v4)) = (ip, *rule_ip) {
                return v6.to_ipv4_mapped() == Some(rule_v4);
            }
            false
        }
        BypassRule::DotSuffix {
            suffix,
            port: rule_port,
        } => {
            if rule_port.is_some_and(|p| p != port) {
                return false;
            }
            host == suffix || is_subdomain_of(host, suffix)
        }
        BypassRule::WildcardSubdomain {
            suffix,
            port: rule_port,
        } => {
            if rule_port.is_some_and(|p| p != port) {
                return false;
            }
            is_subdomain_of(host, suffix)
        }
        BypassRule::ExactHost {
            host: rule_host,
            port: rule_port,
        } => {
            if rule_port.is_some_and(|p| p != port) {
                return false;
            }
            host == rule_host
        }
    }
}

fn is_subdomain_of(host: &str, suffix: &str) -> bool {
    host.len() > suffix.len()
        && host.ends_with(suffix)
        && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
}

fn parse_rule(raw: &str) -> Result<BypassRule, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("旁路规则不能为空".into());
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!("旁路规则 `{trimmed}` 包含非法空白或控制字符"));
    }
    if trimmed == "*" {
        return Ok(BypassRule::Any);
    }
    if trimmed.eq_ignore_ascii_case("<local>") {
        return Ok(BypassRule::Local);
    }

    // CIDR (standard IPv4/IPv6 or macOS shorthand like `169.254/16`, `10/8`).
    if trimmed.contains('/') {
        let net = parse_cidr(trimmed)?;
        return Ok(BypassRule::Cidr(net));
    }

    // Bracketed IPv6 with optional `:port`.
    if let Some(rest) = trimmed.strip_prefix('[') {
        let (ipv6_str, tail) = rest
            .split_once(']')
            .ok_or_else(|| format!("旁路规则 `{trimmed}` 缺少右括号 `]`"))?;
        let ip: Ipv6Addr = ipv6_str
            .parse()
            .map_err(|_| format!("旁路规则 `{trimmed}` 不是有效的 IPv6 地址"))?;
        let port = if tail.is_empty() {
            None
        } else {
            let port_str = tail
                .strip_prefix(':')
                .ok_or_else(|| format!("旁路规则 `{trimmed}` 端口格式无效"))?;
            Some(parse_port(port_str, trimmed)?)
        };
        return Ok(BypassRule::Ip {
            ip: IpAddr::V6(ip),
            port,
        });
    }

    // Unbracketed IPv6 literal (no port).
    if let Ok(ipv6) = trimmed.parse::<Ipv6Addr>() {
        return Ok(BypassRule::Ip {
            ip: IpAddr::V6(ipv6),
            port: None,
        });
    }

    // Optional `:port` for IPv4, suffix, wildcard, or exact hostname.
    let (host_part, port) = match trimmed.rsplit_once(':') {
        Some((left, port_str)) if !left.contains(':') => {
            (left, Some(parse_port(port_str, trimmed)?))
        }
        Some(_) => return Err(format!("旁路规则 `{trimmed}` 格式无效")),
        None => (trimmed, None),
    };

    if let Ok(ipv4) = host_part.parse::<Ipv4Addr>() {
        return Ok(BypassRule::Ip {
            ip: IpAddr::V4(ipv4),
            port,
        });
    }

    if let Some(rest) = host_part.strip_prefix("*.") {
        let suffix = normalize_domain_pattern(rest, trimmed)?;
        return Ok(BypassRule::WildcardSubdomain { suffix, port });
    }
    if let Some(rest) = host_part.strip_prefix('.') {
        let suffix = normalize_domain_pattern(rest, trimmed)?;
        return Ok(BypassRule::DotSuffix { suffix, port });
    }

    let host = normalize_domain_pattern(host_part, trimmed)?;
    Ok(BypassRule::ExactHost { host, port })
}

fn parse_port(port_str: &str, full_rule: &str) -> Result<u16, String> {
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("旁路规则 `{full_rule}` 端口无效"))?;
    if port == 0 {
        return Err(format!("旁路规则 `{full_rule}` 端口必须大于 0"));
    }
    Ok(port)
}

fn normalize_domain_pattern(raw: &str, full_rule: &str) -> Result<String, String> {
    let cleaned = raw.trim_end_matches('.').to_ascii_lowercase();
    if cleaned.is_empty()
        || cleaned.starts_with('.')
        || cleaned.contains("..")
        || cleaned.contains('*')
        || cleaned.contains('/')
    {
        return Err(format!("旁路规则 `{full_rule}` 域名模式无效"));
    }
    Ok(cleaned)
}

fn parse_cidr(raw: &str) -> Result<IpNet, String> {
    if let Ok(net) = raw.parse::<IpNet>() {
        return Ok(net.trunc());
    }
    // macOS shorthand IPv4 CIDR, e.g. `169.254/16`, `10/8`, `192.168.1/24`.
    let (addr_part, prefix_str) = raw
        .split_once('/')
        .ok_or_else(|| format!("旁路 CIDR `{raw}` 格式无效"))?;
    if addr_part.contains(':') {
        return Err(format!("旁路 IPv6 CIDR `{raw}` 格式无效"));
    }
    let prefix: u8 = prefix_str
        .parse()
        .map_err(|_| format!("旁路 CIDR `{raw}` 前缀长度无效"))?;
    let octets: Vec<u8> = addr_part
        .split('.')
        .map(|part| part.parse::<u8>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("旁路 CIDR `{raw}` IPv4 地址无效"))?;
    if octets.is_empty() || octets.len() > 4 {
        return Err(format!("旁路 CIDR `{raw}` IPv4 地址无效"));
    }
    let mut full = [0u8; 4];
    for (idx, octet) in octets.into_iter().enumerate() {
        full[idx] = octet;
    }
    let v4 = Ipv4Addr::from(full);
    let net = Ipv4Net::new(v4, prefix)
        .map_err(|_| format!("旁路 CIDR `{raw}` 前缀长度超出 0..=32"))?;
    Ok(IpNet::V4(net.trunc()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mandatory_loopback_detection() {
        for host in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "api.localhost",
            "a.b.localhost.",
            "127.0.0.1",
            "127.255.254.1",
            "::1",
            "[::1]",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_loopback_host(host), "{host} should be loopback");
        }
        for host in [
            "notlocalhost",
            "localhost.example.com",
            "128.0.0.1",
            "10.0.0.1",
            "::2",
            "imap.gmail.com",
        ] {
            assert!(!is_loopback_host(host), "{host} should not be loopback");
        }
    }

    #[test]
    fn bypass_rules_cover_all_syntaxes() {
        let list = BypassList::parse_strict(&[
            "exact.example.com".into(),
            ".corp.example".into(),
            "*.local".into(),
            "mail.port.test:993".into(),
            "192.168.1.5".into(),
            "192.168.1.6:8080".into(),
            "[fe80::1]:443".into(),
            "2001:db8::5".into(),
            "10.0.0.0/8".into(),
            "169.254/16".into(),
            "172.16/12".into(),
            "fc00::/7".into(),
            "<local>".into(),
        ])
        .unwrap();

        // Exact host
        assert!(list.matches("exact.example.com", 993));
        assert!(list.matches("EXACT.EXAMPLE.COM.", 443));
        assert!(!list.matches("sub.exact.example.com", 993));

        // Leading-dot suffix: apex + subdomains, never partial label
        assert!(list.matches("corp.example", 443));
        assert!(list.matches("imap.corp.example", 993));
        assert!(list.matches("a.b.corp.example", 993));
        assert!(!list.matches("evilcorp.example", 993));

        // Wildcard subdomain: subdomains only (use a multi-label host so `<local>` doesn't match)
        assert!(list.matches("printer.local", 80));
        assert!(list.matches("a.b.local", 80));
        assert!(!list.matches("foo.notlocal", 80));
        let wildcard_only = BypassList::parse_strict(&["*.example.org".into()]).unwrap();
        assert!(wildcard_only.matches("imap.example.org", 993));
        assert!(!wildcard_only.matches("example.org", 993));
        assert!(!wildcard_only.matches("badexample.org", 993));

        // Port-scoped host
        assert!(list.matches("mail.port.test", 993));
        assert!(!list.matches("mail.port.test", 143));

        // IPv4 / IPv6 + port
        assert!(list.matches("192.168.1.5", 25));
        assert!(list.matches("::ffff:192.168.1.5", 25));
        assert!(list.matches("192.168.1.6", 8080));
        assert!(!list.matches("192.168.1.6", 8081));
        assert!(list.matches("fe80::1", 443));
        assert!(list.matches("[fe80::1]", 443));
        assert!(!list.matches("fe80::1", 80));
        assert!(list.matches("2001:db8::5", 993));

        // CIDR including macOS shorthand
        assert!(list.matches("10.25.3.4", 443));
        assert!(list.matches("169.254.169.254", 80));
        assert!(list.matches("172.20.1.2", 993));
        assert!(!list.matches("172.32.0.1", 993));
        assert!(list.matches("fd12:3456::1", 443));

        // <local> simple hostnames
        assert!(list.matches("intranet", 80));
        assert!(!list.matches("imap.gmail.com", 993));
    }

    #[test]
    fn star_matches_everything_and_invalid_rules_are_rejected() {
        let star = BypassList::parse_strict(&["*".into()]).unwrap();
        assert!(star.matches("imap.gmail.com", 993));
        assert!(star.matches("8.8.8.8", 443));

        for bad in [
            "bad host",
            "10.0.0.0/33",
            "example.com:0",
            "example.com:99999",
            "a.*.com",
            "[::1",
            "..example.com",
        ] {
            assert!(
                BypassList::parse_strict(&[bad.into()]).is_err(),
                "{bad:?} should fail strict parse"
            );
        }
    }
}
